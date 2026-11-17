//! Caller assertions: a short-lived statement, signed by this instance, of who
//! called the running script and with which key.
//!
//! A script that calls another service on its caller's behalf can ask for one,
//! addressed to that service. The service then learns the caller from this
//! instance rather than from whatever the script chose to send. The instance,
//! not the script, fills in every claim about the caller:
//!
//! - `sub`, the caller's DID;
//! - `cnf.jkt`, the thumbprint of the DPoP key that authenticated the request;
//! - `azp`, the OAuth client id of the API client the caller signed in through;
//! - `lxm`, the running script's own method.
//!
//! The script picks only `aud`, which must be one of the audiences the
//! instance allows. Only a caller authenticated with DPoP gets one, because a
//! cookie session has no key to name.
//!
//! Off unless `CALLER_ASSERTION_KEY` and `CALLER_ASSERTION_AUDIENCES` are both
//! set. The signing key lives in the instance's settings and never reaches a
//! script; the service pins its public half.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde_json::{Value, json};

/// The JWS `typ` of a caller assertion.
pub const ASSERTION_TYP: &str = "hv-caller+jwt";

/// How long an assertion is valid. It is used at once, by the script that
/// asked for it, so this only covers the call it travels on.
pub const ASSERTION_LIFETIME_SECS: u64 = 30;

/// This instance's assertion key and the audiences it may address.
#[derive(Clone)]
pub struct CallerAssertionConfig {
    kid: String,
    key: SigningKey,
    audiences: Vec<String>,
}

impl std::fmt::Debug for CallerAssertionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallerAssertionConfig")
            .field("kid", &self.kid)
            .field("audiences", &self.audiences)
            .finish_non_exhaustive()
    }
}

impl CallerAssertionConfig {
    /// Parse the two settings. Both unset or blank turns assertions off; one
    /// without the other, or either invalid, is an operator error.
    pub fn parse(key: Option<&str>, audiences: Option<&str>) -> Result<Option<Self>, String> {
        fn blank(v: Option<&str>) -> Option<&str> {
            v.map(str::trim).filter(|s| !s.is_empty())
        }
        let (key, audiences) = match (blank(key), blank(audiences)) {
            (None, None) => return Ok(None),
            (Some(k), Some(a)) => (k, a),
            _ => {
                return Err(
                    "CALLER_ASSERTION_KEY and CALLER_ASSERTION_AUDIENCES must be set together"
                        .into(),
                );
            }
        };
        let jwk: Value = serde_json::from_str(key)
            .map_err(|e| format!("CALLER_ASSERTION_KEY is not a JSON JWK: {e}"))?;
        let kid = jwk["kid"]
            .as_str()
            .filter(|k| !k.is_empty())
            .ok_or("CALLER_ASSERTION_KEY needs a kid")?
            .to_string();
        let key = p256_private_key(&jwk).map_err(|e| format!("CALLER_ASSERTION_KEY: {e}"))?;
        let audiences: Vec<String> = audiences
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .collect();
        if audiences.is_empty() {
            return Err("CALLER_ASSERTION_AUDIENCES names no audience".into());
        }
        Ok(Some(Self {
            kid,
            key,
            audiences,
        }))
    }

    /// The public key set a service pins to verify this instance's
    /// assertions.
    pub fn public_jwks(&self) -> Value {
        let point = self.key.verifying_key().to_sec1_point(false);
        json!({
            "keys": [{
                "kty": "EC",
                "crv": "P-256",
                "x": URL_SAFE_NO_PAD.encode(point.x().expect("uncompressed point")),
                "y": URL_SAFE_NO_PAD.encode(point.y().expect("uncompressed point")),
                "kid": self.kid,
                "alg": "ES256",
                "use": "sig",
            }]
        })
    }
}

/// A P-256 signing key from a private JWK whose public coordinates match it.
fn p256_private_key(jwk: &Value) -> Result<SigningKey, String> {
    if jwk["kty"].as_str() != Some("EC") || jwk["crv"].as_str() != Some("P-256") {
        return Err("must be an EC P-256 key".into());
    }
    let field = |name: &str| -> Result<Vec<u8>, String> {
        let raw = jwk[name].as_str().ok_or(format!("missing {name}"))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| format!("{name} is not base64url"))?;
        if bytes.len() != 32 {
            return Err(format!("{name} must be 32 bytes"));
        }
        Ok(bytes)
    };
    let key = SigningKey::from_slice(&field("d")?).map_err(|_| "d is not a P-256 scalar")?;
    let point = key.verifying_key().to_sec1_point(false);
    if point.x().map(|x| x.to_vec()) != Some(field("x")?)
        || point.y().map(|y| y.to_vec()) != Some(field("y")?)
    {
        return Err("x and y don't match d".into());
    }
    Ok(key)
}

/// Why an assertion was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssertionError {
    #[error("caller assertions are not turned on for this instance")]
    Disabled,
    #[error("'{0}' is not an audience this instance may address")]
    Audience(String),
    #[error("a caller assertion needs a caller who authenticated with DPoP")]
    NotDpop,
    #[error("a caller assertion can only be made by an XRPC script")]
    NotXrpc,
    #[error("a test run's caller is forged, so it gets no caller assertion")]
    TestRun,
    #[error("a run may ask for at most {0} caller assertions")]
    TooMany(u32),
    #[error("{0}")]
    Database(String),
}

impl AssertionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "UNSUPPORTED",
            Self::Audience(_) => "FORBIDDEN",
            Self::NotDpop => "NO_SESSION",
            Self::NotXrpc => "UNSUPPORTED",
            Self::TestRun => "UNSUPPORTED",
            Self::TooMany(_) => "RATE_LIMITED",
            Self::Database(_) => "HOST_ERROR",
        }
    }
}

/// Everything about the caller an assertion states, as the instance resolved
/// it from the authenticated request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerFacts {
    pub did: String,
    pub jkt: String,
    pub azp: String,
    pub lxm: String,
}

/// A signed assertion, and when it expires (Unix seconds).
#[derive(Debug, Clone)]
pub struct Assertion {
    pub token: String,
    pub jti: String,
    pub expires_at: u64,
}

/// Sign an assertion about `caller` for `audience`, at `now` (Unix seconds).
pub fn sign(
    config: &CallerAssertionConfig,
    issuer: &str,
    caller: &CallerFacts,
    audience: &str,
    now: u64,
) -> Result<Assertion, AssertionError> {
    if !config.audiences.iter().any(|a| a == audience) {
        return Err(AssertionError::Audience(audience.to_string()));
    }
    let jti = {
        use rand::Rng;
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        URL_SAFE_NO_PAD.encode(bytes)
    };
    let exp = now + ASSERTION_LIFETIME_SECS;
    let header = json!({ "alg": "ES256", "typ": ASSERTION_TYP, "kid": config.kid });
    let claims = json!({
        "iss": issuer,
        "sub": caller.did,
        "cnf": { "jkt": caller.jkt },
        "azp": caller.azp,
        "aud": audience,
        "lxm": caller.lxm,
        "iat": now,
        "exp": exp,
        "jti": jti,
    });
    let encode = |v: &Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap_or_default());
    let input = format!("{}.{}", encode(&header), encode(&claims));
    let sig: Signature = config.key.sign(input.as_bytes());
    Ok(Assertion {
        token: format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes())),
        jti,
        expires_at: exp,
    })
}

/// The method an XRPC trigger runs for: the NSID of `xrpc.query:<nsid>` or
/// `xrpc.procedure:<nsid>`.
pub fn xrpc_method(trigger_id: &str) -> Option<&str> {
    trigger_id
        .strip_prefix("xrpc.procedure:")
        .or_else(|| trigger_id.strip_prefix("xrpc.query:"))
        .filter(|m| !m.is_empty())
}

/// Resolve what an assertion states about the caller of a script run from the
/// request's own authentication: the DPoP key and API client it used.
pub async fn caller_facts(
    state: &crate::AppState,
    claims: &crate::auth::Claims,
    trigger_id: &str,
) -> Result<CallerFacts, AssertionError> {
    let lxm = xrpc_method(trigger_id).ok_or(AssertionError::NotXrpc)?;
    let (Some(dpop_key_id), Some(client_key)) = (claims.dpop_key_id(), claims.client_key()) else {
        return Err(AssertionError::NotDpop);
    };
    let db = |e: sqlx::Error| AssertionError::Database(format!("caller assertion lookup: {e}"));
    let backend = state.db_backend;
    let jkt: Option<(String,)> = crate::db::query_as(&crate::db::adapt_sql(
        "SELECT jwk_thumbprint FROM happyview_dpop_keys WHERE id = ?",
        backend,
    ))
    .bind(dpop_key_id)
    .fetch_optional(&state.db)
    .await
    .map_err(db)?;
    let azp: Option<(String,)> = crate::db::query_as(&crate::db::adapt_sql(
        "SELECT client_id_url FROM happyview_api_clients WHERE client_key = ?",
        backend,
    ))
    .bind(client_key)
    .fetch_optional(&state.db)
    .await
    .map_err(db)?;
    let (Some((jkt,)), Some((azp,))) = (jkt, azp) else {
        return Err(AssertionError::NotDpop);
    };
    Ok(CallerFacts {
        did: claims.did().to_string(),
        jkt,
        azp,
        lxm: lxm.to_string(),
    })
}

/// This instance's issuer name: its public URL without a trailing slash.
pub fn issuer(state: &crate::AppState) -> String {
    state.config.public_url.trim_end_matches('/').to_string()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Make an assertion for the caller of the script run `trigger_id`, addressed
/// to `audience`, and record it in the event log.
pub async fn assert_caller(
    state: &crate::AppState,
    claims: &crate::auth::Claims,
    trigger_id: &str,
    audience: &str,
) -> Result<Assertion, AssertionError> {
    let config = state
        .config
        .caller_assertions
        .as_ref()
        .ok_or(AssertionError::Disabled)?;
    if !config.audiences.iter().any(|a| a == audience) {
        return Err(AssertionError::Audience(audience.to_string()));
    }
    let caller = caller_facts(state, claims, trigger_id).await?;
    let assertion = sign(config, &issuer(state), &caller, audience, now_secs())?;
    crate::event_log::log_event(
        &state.db,
        crate::event_log::EventLog {
            event_type: "caller_assertion.issued".to_string(),
            severity: crate::event_log::Severity::Info,
            actor_did: Some(caller.did.clone()),
            subject: Some(trigger_id.to_string()),
            detail: json!({
                "aud": audience,
                "lxm": caller.lxm,
                "azp": caller.azp,
                "jkt": caller.jkt,
                "jti": assertion.jti,
            }),
        },
        state.db_backend,
    )
    .await;
    Ok(assertion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{VerifyingKey, signature::Verifier};

    fn private_jwk(key: &SigningKey, kid: &str) -> String {
        let point = key.verifying_key().to_sec1_point(false);
        json!({
            "kty": "EC", "crv": "P-256", "kid": kid,
            "x": URL_SAFE_NO_PAD.encode(point.x().unwrap()),
            "y": URL_SAFE_NO_PAD.encode(point.y().unwrap()),
            "d": URL_SAFE_NO_PAD.encode(key.to_bytes()),
        })
        .to_string()
    }

    fn new_key() -> SigningKey {
        use rand::Rng;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        SigningKey::from_slice(&bytes).unwrap()
    }

    const AUD: &str = "https://service.example";

    fn config(key: &SigningKey) -> CallerAssertionConfig {
        CallerAssertionConfig::parse(Some(&private_jwk(key, "a1")), Some(&format!(" {AUD} ,")))
            .unwrap()
            .unwrap()
    }

    fn facts() -> CallerFacts {
        CallerFacts {
            did: "did:plc:caller".into(),
            jkt: "jkt-1".into(),
            azp: "https://app.example/client-metadata.json".into(),
            lxm: "com.example.getThing".into(),
        }
    }

    fn decode(part: &str) -> Value {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    }

    #[test]
    fn an_assertion_states_the_caller_and_verifies_against_the_published_key() {
        let key = new_key();
        let config = config(&key);
        let a = sign(
            &config,
            "https://instance.example",
            &facts(),
            AUD,
            1_800_000_000,
        )
        .unwrap();

        let parts: Vec<&str> = a.token.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(
            decode(parts[0]),
            json!({ "alg": "ES256", "typ": ASSERTION_TYP, "kid": "a1" })
        );
        let claims = decode(parts[1]);
        assert_eq!(claims["iss"], "https://instance.example");
        assert_eq!(claims["sub"], "did:plc:caller");
        assert_eq!(claims["cnf"]["jkt"], "jkt-1");
        assert_eq!(claims["azp"], "https://app.example/client-metadata.json");
        assert_eq!(claims["aud"], AUD);
        assert_eq!(claims["lxm"], "com.example.getThing");
        assert_eq!(claims["iat"], 1_800_000_000u64);
        assert_eq!(claims["exp"], 1_800_000_030u64);
        assert_eq!(claims["jti"], a.jti.as_str());
        assert_eq!(URL_SAFE_NO_PAD.decode(&a.jti).unwrap().len(), 16);

        let jwk = &config.public_jwks()["keys"][0];
        assert!(jwk.get("d").is_none());
        let mut sec1 = vec![0x04];
        sec1.extend(URL_SAFE_NO_PAD.decode(jwk["x"].as_str().unwrap()).unwrap());
        sec1.extend(URL_SAFE_NO_PAD.decode(jwk["y"].as_str().unwrap()).unwrap());
        let public = VerifyingKey::from_sec1_bytes(&sec1).unwrap();
        let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        let input = format!("{}.{}", parts[0], parts[1]);
        assert!(public.verify(input.as_bytes(), &sig).is_ok());
    }

    #[test]
    fn only_allowed_audiences_are_addressed() {
        let config = config(&new_key());
        assert_eq!(
            sign(
                &config,
                "https://i.example",
                &facts(),
                "https://other.example",
                0
            )
            .unwrap_err(),
            AssertionError::Audience("https://other.example".into())
        );
    }

    #[test]
    fn settings_must_be_complete_and_consistent() {
        assert!(CallerAssertionConfig::parse(None, None).unwrap().is_none());
        assert!(
            CallerAssertionConfig::parse(Some(" "), Some(""))
                .unwrap()
                .is_none()
        );
        let key = new_key();
        let jwk = private_jwk(&key, "a1");
        assert!(CallerAssertionConfig::parse(Some(&jwk), None).is_err());
        assert!(CallerAssertionConfig::parse(None, Some(AUD)).is_err());
        assert!(CallerAssertionConfig::parse(Some(&jwk), Some(" , ")).is_err());

        // A public key alone, or one whose coordinates belong to another key.
        let mut public: Value = serde_json::from_str(&jwk).unwrap();
        public.as_object_mut().unwrap().remove("d");
        assert!(CallerAssertionConfig::parse(Some(&public.to_string()), Some(AUD)).is_err());
        let mut mismatched: Value = serde_json::from_str(&jwk).unwrap();
        mismatched["d"] = json!(URL_SAFE_NO_PAD.encode(new_key().to_bytes()));
        assert!(CallerAssertionConfig::parse(Some(&mismatched.to_string()), Some(AUD)).is_err());

        let mut no_kid: Value = serde_json::from_str(&jwk).unwrap();
        no_kid.as_object_mut().unwrap().remove("kid");
        assert!(CallerAssertionConfig::parse(Some(&no_kid.to_string()), Some(AUD)).is_err());

        // The key never shows up in a debug print.
        let printed = format!("{:?}", config(&key));
        assert!(!printed.contains(&URL_SAFE_NO_PAD.encode(key.to_bytes())));
    }

    #[test]
    fn the_method_comes_from_an_xrpc_trigger() {
        assert_eq!(
            xrpc_method("xrpc.procedure:com.example.getThing"),
            Some("com.example.getThing")
        );
        assert_eq!(
            xrpc_method("xrpc.query:com.example.q"),
            Some("com.example.q")
        );
        assert_eq!(xrpc_method("record.create:com.example.r"), None);
        assert_eq!(xrpc_method("job.run:x"), None);
        assert_eq!(xrpc_method("xrpc.procedure:"), None);
    }

    /// An API client and DPoP key, as a DPoP sign-in leaves them; the claims
    /// a request authenticated with them carries.
    async fn seed_dpop_caller(state: &crate::AppState, did: &str) -> (crate::auth::Claims, String) {
        use crate::db::adapt_sql;
        let now = crate::db::now_rfc3339();
        let client_id = uuid::Uuid::new_v4().to_string();
        let client_key = format!("hvc_{}", uuid::Uuid::new_v4().simple());
        let client_id_url = format!("https://client.test/{client_id}");
        let dpop_key_id = uuid::Uuid::new_v4().to_string();
        let thumbprint = format!("tp-{dpop_key_id}");
        crate::db::query(&adapt_sql(
            "INSERT INTO happyview_api_clients (id, client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, scopes, client_type, is_active, created_by, created_at, updated_at) VALUES (?, ?, '', 'test', ?, '', '[]', '', 'public', 1, ?, ?, ?)",
            state.db_backend,
        ))
        .bind(&client_id)
        .bind(&client_key)
        .bind(&client_id_url)
        .bind(did)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .expect("seed api client");
        crate::db::query(&adapt_sql(
            "INSERT INTO happyview_dpop_keys (id, provision_id, api_client_id, private_key_enc, jwk_thumbprint, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            state.db_backend,
        ))
        .bind(&dpop_key_id)
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&client_id)
        .bind(vec![0u8])
        .bind(&thumbprint)
        .bind(&now)
        .execute(&state.db)
        .await
        .expect("seed dpop key");
        let claims = crate::auth::Claims::for_test(did, Some(&client_key), Some(&dpop_key_id));
        (claims, format!("{thumbprint}|{client_id_url}"))
    }

    #[tokio::test]
    async fn the_instance_fills_in_the_caller_from_the_request() {
        let Some(mut state) = crate::test_support::test_state_from_env().await else {
            return;
        };
        let did = format!("did:plc:{}", uuid::Uuid::new_v4().simple());
        let trigger = "xrpc.procedure:com.example.getThing";

        state.config.caller_assertions = None;
        let (claims, expected) = seed_dpop_caller(&state, &did).await;
        assert_eq!(
            assert_caller(&state, &claims, trigger, AUD)
                .await
                .unwrap_err(),
            AssertionError::Disabled
        );

        state.config.caller_assertions = Some(config(&new_key()));
        let a = assert_caller(&state, &claims, trigger, AUD).await.unwrap();
        let claims_out = decode(a.token.split('.').nth(1).unwrap());
        let (thumbprint, client_id_url) = expected.split_once('|').unwrap();
        assert_eq!(claims_out["sub"], did.as_str());
        assert_eq!(claims_out["cnf"]["jkt"], thumbprint);
        assert_eq!(claims_out["azp"], client_id_url);
        assert_eq!(claims_out["lxm"], "com.example.getThing");
        assert_eq!(claims_out["iss"], issuer(&state).as_str());

        // Recorded, without the token.
        let (detail,): (String,) = crate::db::query_as(&crate::db::adapt_sql(
            "SELECT detail FROM happyview_event_logs WHERE event_type = 'caller_assertion.issued' AND actor_did = ?",
            state.db_backend,
        ))
        .bind(&did)
        .fetch_one(&state.db)
        .await
        .unwrap();
        let detail: Value = serde_json::from_str(&detail).unwrap();
        assert_eq!(detail["jti"], a.jti.as_str());
        assert!(!detail.to_string().contains(&a.token));

        assert_eq!(
            assert_caller(&state, &claims, trigger, "https://other.example")
                .await
                .unwrap_err(),
            AssertionError::Audience("https://other.example".into())
        );
        assert_eq!(
            assert_caller(&state, &claims, "record.create:com.example.r", AUD)
                .await
                .unwrap_err(),
            AssertionError::NotXrpc
        );
        // A cookie session has no key to name.
        let cookie = crate::auth::Claims::for_test(&did, None, None);
        assert_eq!(
            assert_caller(&state, &cookie, trigger, AUD)
                .await
                .unwrap_err(),
            AssertionError::NotDpop
        );
        // A test run's forged caller carries no DPoP key, so it gets none
        // either, even where the run itself is let through.
        let forged = crate::auth::Claims::internal(did.clone());
        assert_eq!(
            assert_caller(&state, &forged, trigger, AUD)
                .await
                .unwrap_err(),
            AssertionError::NotDpop
        );
        // A job that inherited a DPoP caller isn't an XRPC call, so it has no
        // method to name.
        assert_eq!(
            assert_caller(&state, &claims, "job:com.example.tick", AUD)
                .await
                .unwrap_err(),
            AssertionError::NotXrpc
        );
        // Nor does a key this instance doesn't know.
        let unknown = crate::auth::Claims::for_test(&did, Some("hvc_unknown"), Some("nope"));
        assert_eq!(
            assert_caller(&state, &unknown, trigger, AUD)
                .await
                .unwrap_err(),
            AssertionError::NotDpop
        );
    }
}
