//! Management tickets: a trusted issuer's short-lived statement that one user
//! may manage this instance, presented together with a proof for the request
//! signed by the key the ticket names.
//!
//! A hosting platform that already knows who owns an instance can let its own
//! app manage that instance as the signed-in user, without the user signing in
//! to each instance. The platform issues a ticket naming the user, this
//! instance and the caller's key (`cnf.jkt`); the caller sends it with
//! `Authorization: HV-Ticket <ticket>` and an `HV-Ticket-Proof` header, a
//! DPoP-style JWS binding the ticket to one request. The instance checks both,
//! then treats the caller as that user, with that user's own permissions.
//!
//! Off unless `MANAGEMENT_TICKET_ISSUER`, `MANAGEMENT_TICKET_AUDIENCE` and
//! `MANAGEMENT_TICKET_JWKS` are all set. The issuer's keys are pinned there,
//! so verifying a ticket fetches nothing.
//!
//! A ticket opens the admin API only. It never becomes [`Claims`], so it
//! cannot reach anything that acts with a stored OAuth session, and it never
//! makes its user the instance's first super user.
//!
//! [`Claims`]: crate::auth::Claims

use std::collections::HashMap;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The `Authorization` scheme, with its separating space.
pub const SCHEME: &str = "HV-Ticket ";

/// The header carrying the request proof.
pub const PROOF_HEADER: &str = "hv-ticket-proof";

const TICKET_TYP: &str = "hv-ticket+jwt";
const PROOF_TYP: &str = "hv-ticket-proof+jwt";
const TICKET_VERSION: u64 = 1;

/// Leeway on the ticket's own times, for the gap between the issuer's clock
/// and this instance's.
const TICKET_LEEWAY_SECS: u64 = 30;

/// The longest lifetime a ticket may claim: the 60 seconds issuers sign for.
/// A ticket claiming more is refused rather than trusted for longer; clock
/// differences are covered by [`TICKET_LEEWAY_SECS`], not by this.
const MAX_TICKET_LIFETIME_SECS: u64 = 60;

/// How far a proof's `iat` may be from this instance's clock. Loose on
/// purpose: a proof is bound to its ticket through `ath`, so it can never
/// outlive the ticket, and a browser's clock is often a minute or two out.
const PROOF_IAT_TOLERANCE_SECS: u64 = 300;

/// The largest request body buffered for hashing: the project bundle limit,
/// the largest body any admin route accepts.
pub const MAX_BODY_BYTES: usize = crate::project::deploy::bundle::MAX_COMPRESSED_BYTES;

/// The issuer this instance trusts, from its settings.
#[derive(Clone)]
pub struct TicketTrust {
    pub issuer: String,
    pub audience: String,
    keys: HashMap<String, VerifyingKey>,
}

impl std::fmt::Debug for TicketTrust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut kids: Vec<&String> = self.keys.keys().collect();
        kids.sort();
        f.debug_struct("TicketTrust")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("kids", &kids)
            .finish()
    }
}

impl TicketTrust {
    /// Parse the three settings. All unset or blank turns tickets off; some
    /// set and others not, or any of them invalid, is an operator error.
    pub fn parse(
        issuer: Option<&str>,
        audience: Option<&str>,
        jwks: Option<&str>,
    ) -> Result<Option<Self>, String> {
        fn blank(v: Option<&str>) -> Option<&str> {
            v.map(str::trim).filter(|s| !s.is_empty())
        }
        let (issuer, audience, jwks) = match (blank(issuer), blank(audience), blank(jwks)) {
            (None, None, None) => return Ok(None),
            (Some(i), Some(a), Some(j)) => (i, a, j),
            _ => {
                return Err("MANAGEMENT_TICKET_ISSUER, MANAGEMENT_TICKET_AUDIENCE and \
                     MANAGEMENT_TICKET_JWKS must be set together"
                    .into());
            }
        };

        let doc: serde_json::Value = serde_json::from_str(jwks)
            .map_err(|e| format!("MANAGEMENT_TICKET_JWKS is not JSON: {e}"))?;
        let list = doc["keys"]
            .as_array()
            .ok_or("MANAGEMENT_TICKET_JWKS must be a key set: {\"keys\": [...]}")?;
        let mut keys = HashMap::new();
        for jwk in list {
            let kid = jwk["kid"]
                .as_str()
                .filter(|k| !k.is_empty())
                .ok_or("every key in MANAGEMENT_TICKET_JWKS needs a kid")?;
            if jwk.get("d").is_some() {
                return Err(format!(
                    "MANAGEMENT_TICKET_JWKS key {kid} holds a private key; publish only the public key"
                ));
            }
            let key = p256_public_key(jwk)
                .map_err(|e| format!("MANAGEMENT_TICKET_JWKS key {kid}: {e}"))?;
            if keys.insert(kid.to_string(), key).is_some() {
                return Err(format!("MANAGEMENT_TICKET_JWKS lists kid {kid} twice"));
            }
        }
        if keys.is_empty() {
            return Err("MANAGEMENT_TICKET_JWKS has no keys".into());
        }
        Ok(Some(TicketTrust {
            issuer: issuer.to_string(),
            audience: audience.to_string(),
            keys,
        }))
    }
}

/// Why a ticket request was refused. The step names are logged; neither the
/// ticket nor the proof ever is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Tickets are not turned on here.
    Disabled,
    /// The request also carries another credential (a cookie, `DPoP`, a client
    /// key) or more than one `Authorization` header.
    MixedCredentials,
    /// The request asks to upgrade the connection.
    Upgrade,
    MissingProof,
    Ticket(&'static str),
    Proof(&'static str),
    Replay,
    /// The ticket's user is not a user of this instance.
    NotAUser,
}

impl Refusal {
    pub fn step(&self) -> &'static str {
        match self {
            Refusal::Disabled => "disabled",
            Refusal::MixedCredentials => "mixed_credentials",
            Refusal::Upgrade => "upgrade",
            Refusal::MissingProof => "missing_proof",
            Refusal::Ticket(s) => s,
            Refusal::Proof(s) => s,
            Refusal::Replay => "replay",
            Refusal::NotAUser => "not_a_user",
        }
    }

    pub fn into_app_error(self) -> crate::error::AppError {
        use crate::error::AppError;
        match self {
            Refusal::Disabled => AppError::Auth("invalid Authorization scheme".into()),
            Refusal::MixedCredentials => AppError::BadRequest(
                "a request with an HV-Ticket must carry no other credential".into(),
            ),
            Refusal::Upgrade => {
                AppError::BadRequest("an HV-Ticket request cannot upgrade the connection".into())
            }
            Refusal::MissingProof => AppError::Auth("HV-Ticket requires HV-Ticket-Proof".into()),
            Refusal::Ticket("expired") => AppError::Auth("ticket_expired".into()),
            Refusal::Ticket(_) => AppError::Auth("invalid ticket".into()),
            Refusal::Proof(_) => AppError::Auth("invalid ticket proof".into()),
            Refusal::Replay => AppError::Auth("ticket proof replay".into()),
            Refusal::NotAUser => AppError::Forbidden("not a user".into()),
        }
    }
}

/// What the request looked like, for checking the proof against it.
pub struct RequestFacts<'a> {
    pub method: &'a str,
    /// The path as the client sent it, before any router stripped a prefix.
    pub path: &'a str,
    /// The raw query string, without `?`; empty when there is none.
    pub query: &'a str,
    pub content_type: Option<&'a str>,
}

/// A ticket and proof that verified. [`gate`] leaves it in the request's
/// extensions; the admin extractor then checks that `did` is a user.
#[derive(Debug, Clone)]
pub struct Verified {
    pub did: String,
    pub jkt: String,
    pub ticket_jti: String,
    pub issuer: String,
    pub htu_origin: String,
}

/// What [`verify_head`] verified from the headers alone, and what is left to
/// check once the body has been read.
pub struct Head {
    pub verified: Verified,
    /// The proof's body hash, still to be compared with the body.
    bh: String,
    proof_jti: String,
    /// When the proof's `jti` may be forgotten: the ticket's expiry plus
    /// leeway, after which the ticket itself is refused.
    pub forget_at: u64,
}

impl Head {
    /// Compare the proof's body hash with the body's.
    pub fn check_body(&self, body_sha256: &[u8; 32]) -> Result<(), Refusal> {
        if self.bh == URL_SAFE_NO_PAD.encode(body_sha256) {
            Ok(())
        } else {
            Err(Refusal::Proof("body"))
        }
    }

    /// The key the proof's `jti` is recorded under.
    pub fn replay_key(&self) -> String {
        format!("{}:{}", self.verified.jkt, self.proof_jti)
    }
}

#[derive(Deserialize)]
struct TicketHeader {
    alg: String,
    typ: String,
    kid: String,
}

#[derive(Deserialize)]
struct Cnf {
    jkt: String,
}

#[derive(Deserialize)]
struct TicketClaims {
    iss: String,
    sub: String,
    aud: String,
    cnf: Cnf,
    iat: u64,
    exp: u64,
    jti: String,
    v: u64,
}

#[derive(Deserialize)]
struct ProofHeader {
    alg: String,
    typ: String,
    jwk: serde_json::Value,
}

#[derive(Deserialize)]
struct ProofClaims {
    htm: String,
    htu: String,
    hq: String,
    #[serde(default)]
    ct: Option<String>,
    bh: String,
    ath: String,
    iat: u64,
    jti: String,
}

/// Verify a ticket and its proof against `trust` and the request's head, at
/// `now` (Unix seconds): everything except the body hash and replay, which
/// need the body and the database. Nothing here reads the body, so a request
/// that fails costs no buffering.
pub fn verify_head(
    trust: &TicketTrust,
    ticket: &str,
    proof: &str,
    facts: &RequestFacts<'_>,
    now: u64,
) -> Result<Head, Refusal> {
    verify_ticket(trust, ticket, now)?.check_proof(ticket, proof, facts, now)
}

/// A ticket whose signature and claims verified, before its proof is checked.
#[derive(Debug, Clone)]
pub struct VerifiedTicket {
    pub did: String,
    pub jkt: String,
    pub ticket_jti: String,
    pub issuer: String,
    exp: u64,
}

/// Verify the ticket alone: signature, issuer, audience, version and times.
pub fn verify_ticket(
    trust: &TicketTrust,
    ticket: &str,
    now: u64,
) -> Result<VerifiedTicket, Refusal> {
    let t = ticket_claims(trust, ticket, now)?;
    Ok(VerifiedTicket {
        did: t.sub,
        jkt: t.cnf.jkt,
        ticket_jti: t.jti,
        issuer: t.iss,
        exp: t.exp,
    })
}

impl VerifiedTicket {
    /// Check the request's proof against this ticket and the request's head.
    pub fn check_proof(
        &self,
        ticket: &str,
        proof: &str,
        facts: &RequestFacts<'_>,
        now: u64,
    ) -> Result<Head, Refusal> {
        let p = proof_claims(proof, &self.jkt, ticket, facts, now)?;
        Ok(Head {
            verified: Verified {
                did: self.did.clone(),
                jkt: self.jkt.clone(),
                ticket_jti: self.ticket_jti.clone(),
                issuer: self.issuer.clone(),
                htu_origin: p.origin,
            },
            bh: p.bh,
            proof_jti: p.jti,
            forget_at: self.exp + TICKET_LEEWAY_SECS,
        })
    }
}

struct CheckedProof {
    origin: String,
    bh: String,
    jti: String,
}

fn ticket_claims(trust: &TicketTrust, ticket: &str, now: u64) -> Result<TicketClaims, Refusal> {
    let bad = Refusal::Ticket;
    let (header, claims, signing_input, sig) = split_jws(ticket).ok_or(bad("format"))?;
    let header: TicketHeader = serde_json::from_slice(&header).map_err(|_| bad("header"))?;
    if header.alg != "ES256" || header.typ != TICKET_TYP {
        return Err(bad("header"));
    }
    let key = trust.keys.get(&header.kid).ok_or(bad("unknown_kid"))?;
    verify_signature(key, signing_input, &sig).map_err(|_| bad("signature"))?;

    let t: TicketClaims = serde_json::from_slice(&claims).map_err(|_| bad("claims"))?;
    if t.v != TICKET_VERSION {
        return Err(bad("version"));
    }
    if t.iss != trust.issuer {
        return Err(bad("issuer"));
    }
    if t.aud != trust.audience {
        return Err(bad("audience"));
    }
    if !t.sub.starts_with("did:") || t.sub.len() <= "did:".len() {
        return Err(bad("subject"));
    }
    if t.jti.is_empty() || t.cnf.jkt.is_empty() {
        return Err(bad("claims"));
    }
    if t.exp <= t.iat || t.exp - t.iat > MAX_TICKET_LIFETIME_SECS {
        return Err(bad("lifetime"));
    }
    if t.iat > now + TICKET_LEEWAY_SECS {
        return Err(bad("not_yet_valid"));
    }
    if now > t.exp + TICKET_LEEWAY_SECS {
        return Err(bad("expired"));
    }
    Ok(t)
}

/// Check the proof against the request's head.
fn proof_claims(
    proof: &str,
    jkt: &str,
    ticket: &str,
    facts: &RequestFacts<'_>,
    now: u64,
) -> Result<CheckedProof, Refusal> {
    let bad = Refusal::Proof;
    let (header, claims, signing_input, sig) = split_jws(proof).ok_or(bad("format"))?;
    let header: ProofHeader = serde_json::from_slice(&header).map_err(|_| bad("header"))?;
    if header.alg != "ES256" || header.typ != PROOF_TYP {
        return Err(bad("header"));
    }
    if header.jwk.get("d").is_some() {
        return Err(bad("key"));
    }
    let key = p256_public_key(&header.jwk).map_err(|_| bad("key"))?;
    let thumbprint =
        crate::oauth::keys::compute_jwk_thumbprint(&header.jwk).map_err(|_| bad("key"))?;
    if thumbprint != jkt {
        return Err(bad("key_mismatch"));
    }
    verify_signature(&key, signing_input, &sig).map_err(|_| bad("signature"))?;

    let p: ProofClaims = serde_json::from_slice(&claims).map_err(|_| bad("claims"))?;
    if !p.htm.eq_ignore_ascii_case(facts.method) {
        return Err(bad("method"));
    }
    let (origin, path) = split_htu(&p.htu).ok_or(bad("url"))?;
    if path != facts.path {
        return Err(bad("url"));
    }
    if p.hq != b64_sha256(facts.query.as_bytes()) {
        return Err(bad("query"));
    }
    if p.ct.as_deref() != facts.content_type {
        return Err(bad("content_type"));
    }
    if URL_SAFE_NO_PAD.decode(&p.bh).map(|b| b.len()) != Ok(32) {
        return Err(bad("claims"));
    }
    if p.ath != b64_sha256(ticket.as_bytes()) {
        return Err(bad("ticket_binding"));
    }
    if p.iat.abs_diff(now) > PROOF_IAT_TOLERANCE_SECS {
        return Err(bad("time"));
    }
    if p.jti.is_empty() || p.jti.len() > 128 {
        return Err(bad("claims"));
    }
    Ok(CheckedProof {
        origin,
        bh: p.bh,
        jti: p.jti,
    })
}

/// `(origin, path)` of an absolute http(s) URL with no query or fragment.
fn split_htu(htu: &str) -> Option<(String, String)> {
    let url = url::Url::parse(htu).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let origin = url.origin().ascii_serialization();
    Some((origin, url.path().to_string()))
}

/// A compact JWS's decoded header and claims, its signing input and its
/// decoded signature.
type JwsParts<'a> = (Vec<u8>, Vec<u8>, &'a str, Vec<u8>);

/// Split and decode a compact JWS.
fn split_jws(jws: &str) -> Option<JwsParts<'_>> {
    let mut parts = jws.split('.');
    let (h, c, s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let signing_input = &jws[..h.len() + 1 + c.len()];
    Some((
        URL_SAFE_NO_PAD.decode(h).ok()?,
        URL_SAFE_NO_PAD.decode(c).ok()?,
        signing_input,
        URL_SAFE_NO_PAD.decode(s).ok()?,
    ))
}

fn verify_signature(key: &VerifyingKey, signing_input: &str, sig: &[u8]) -> Result<(), ()> {
    let sig = Signature::from_slice(sig).map_err(|_| ())?;
    key.verify(signing_input.as_bytes(), &sig).map_err(|_| ())
}

/// A P-256 public key from a JWK, refusing anything else.
fn p256_public_key(jwk: &serde_json::Value) -> Result<VerifyingKey, String> {
    if jwk["kty"].as_str() != Some("EC") || jwk["crv"].as_str() != Some("P-256") {
        return Err("must be an EC P-256 key".into());
    }
    let coord = |name: &str| -> Result<Vec<u8>, String> {
        let raw = jwk[name].as_str().ok_or(format!("missing {name}"))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| format!("{name} is not base64url"))?;
        if bytes.len() != 32 {
            return Err(format!("{name} must be 32 bytes"));
        }
        Ok(bytes)
    };
    let mut sec1 = Vec::with_capacity(65);
    sec1.push(0x04);
    sec1.extend_from_slice(&coord("x")?);
    sec1.extend_from_slice(&coord("y")?);
    VerifyingKey::from_sec1_bytes(&sec1).map_err(|_| "not a point on P-256".to_string())
}

fn b64_sha256(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(bytes))
}

/// Whether the request presents a ticket.
pub fn presents_ticket(headers: &HeaderMap) -> bool {
    headers
        .get_all(axum::http::header::AUTHORIZATION)
        .iter()
        .any(|v| v.to_str().is_ok_and(|s| s.starts_with(SCHEME)))
}

/// Refuse a ticket request that carries anything else that could decide who
/// the caller is, or that asks to upgrade the connection.
pub fn check_alone(headers: &HeaderMap) -> Result<(), Refusal> {
    use axum::http::header::{AUTHORIZATION, COOKIE, UPGRADE};
    if headers.get_all(AUTHORIZATION).iter().count() != 1
        || headers.contains_key(COOKIE)
        || headers.contains_key("dpop")
        || headers.contains_key("x-client-key")
        || headers.contains_key("x-client-secret")
    {
        return Err(Refusal::MixedCredentials);
    }
    if headers.contains_key(UPGRADE) {
        return Err(Refusal::Upgrade);
    }
    Ok(())
}

/// Record a proof's `jti`: `Ok(false)` when it was already recorded. Backed
/// by the database, so every process serving this instance sees the same set.
/// Expired rows are swept as new ones arrive.
pub async fn record_jti(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    key: &str,
    forget_at: u64,
    now: u64,
) -> Result<bool, crate::error::AppError> {
    use crate::db::adapt_sql;
    crate::db::query(&adapt_sql(
        "DELETE FROM happyview_ticket_proofs WHERE forget_at < ?",
        backend,
    ))
    .bind(now as i64)
    .execute(db)
    .await
    .map_err(|e| crate::error::AppError::Internal(format!("ticket replay sweep failed: {e}")))?;
    match crate::db::query(&adapt_sql(
        "INSERT INTO happyview_ticket_proofs (proof_key, forget_at) VALUES (?, ?)",
        backend,
    ))
    .bind(key)
    .bind(forget_at as i64)
    .execute(db)
    .await
    {
        Ok(_) => Ok(true),
        Err(e)
            if e.as_database_error()
                .is_some_and(|d| d.is_unique_violation()) =>
        {
            Ok(false)
        }
        Err(e) => Err(crate::error::AppError::Internal(format!(
            "ticket replay record failed: {e}"
        ))),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

tokio::task_local! {
    /// The verified ticket behind the request being handled, while its
    /// handler runs. Read by the event log, which stamps the ticket onto every
    /// event the request writes, and by job creation, which refuses a job
    /// that would act with a stored session.
    static TICKET_REQUEST: Verified;
}

/// The verified ticket behind the request being handled, if it presented one.
/// Set by [`gate`] for the handler's own task; work the handler spawns onto
/// another task doesn't see it.
pub fn current() -> Option<Verified> {
    TICKET_REQUEST.try_with(Clone::clone).ok()
}

/// Run `f` as if within a request authenticated by `ticket`.
pub async fn scope<F: std::future::Future>(ticket: Verified, f: F) -> F::Output {
    TICKET_REQUEST.scope(ticket, f).await
}

/// Event-log detail for an event written while handling a ticket request:
/// `detail` with `auth: "ticket"` and the ticket's `jti`, issuer and key
/// thumbprint added. `None` outside a ticket request, where the detail is
/// written as it is.
pub fn event_detail(detail: &serde_json::Value) -> Option<serde_json::Value> {
    current().map(|t| with_ticket_detail(detail, &t.ticket_jti, &t.issuer, &t.jkt))
}

fn with_ticket_detail(
    detail: &serde_json::Value,
    jti: &str,
    issuer: &str,
    jkt: &str,
) -> serde_json::Value {
    use serde_json::{Value, json};
    let mut out = match detail {
        Value::Object(map) => Value::Object(map.clone()),
        Value::Null => json!({}),
        other => json!({ "value": other }),
    };
    out["auth"] = json!("ticket");
    out["ticket"] = json!({ "jti": jti, "iss": issuer, "jkt": jkt });
    out
}

/// Record a refused request whose ticket itself verified: one the trusted
/// issuer signed for this instance, refused at a later step. Requests whose
/// ticket doesn't verify are only traced, so they add nothing to the event
/// log.
pub async fn log_refusal(
    state: &crate::AppState,
    ticket: &VerifiedTicket,
    step: &str,
    method: &str,
    path: &str,
) {
    let detail = with_ticket_detail(
        &serde_json::json!({ "step": step, "method": method, "path": path }),
        &ticket.ticket_jti,
        &ticket.issuer,
        &ticket.jkt,
    );
    crate::event_log::log_event(
        &state.db,
        crate::event_log::EventLog {
            event_type: "ticket.refused".to_string(),
            severity: crate::event_log::Severity::Warn,
            actor_did: Some(ticket.did.clone()),
            subject: None,
            detail,
        },
        state.db_backend,
    )
    .await;
}

/// Verify a request that presents a management ticket, in an order that
/// spends nothing on one that fails: the headers first, then the body (read
/// only once the ticket and proof hold, and only up to the largest body an
/// admin route accepts), then its hash, then replay. A request that passes
/// carries [`Verified`] on to the admin extractor, and runs with it as the
/// current ticket ([`current`]); other requests pass untouched.
pub async fn gate(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    req: Request,
    next: Next,
) -> Response {
    if !presents_ticket(req.headers()) {
        return next.run(req).await;
    }
    let uri = req
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|u| u.0.clone())
        .unwrap_or_else(|| req.uri().clone());
    let path = uri.path();
    let method = req.method().as_str().to_string();
    let refuse = |r: Refusal| {
        tracing::warn!(step = r.step(), path, "refused a management ticket");
        r.into_app_error().into_response()
    };

    let Some(trust) = state.config.management_tickets.as_ref() else {
        return refuse(Refusal::Disabled);
    };
    if let Err(r) = check_alone(req.headers()) {
        return refuse(r);
    }
    let headers = req.headers();
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix(SCHEME))
        .unwrap_or_default()
        .trim()
        .to_string();
    let Some(proof) = headers
        .get(PROOF_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    else {
        return refuse(Refusal::MissingProof);
    };
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let facts = RequestFacts {
        method: &method,
        path,
        query: uri.query().unwrap_or(""),
        content_type: content_type.as_deref(),
    };
    let now = now_secs();
    let ticket = match verify_ticket(trust, &token, now) {
        Ok(t) => t,
        Err(r) => return refuse(r),
    };

    // From here on the ticket is the issuer's, so a refusal is also recorded
    // in the event log.
    let head = match ticket.check_proof(&token, &proof, &facts, now) {
        Ok(h) => h,
        Err(r) => {
            log_refusal(&state, &ticket, r.step(), &method, path).await;
            return refuse(r);
        }
    };
    let own_origin = url::Url::parse(&state.config.public_url)
        .map(|u| u.origin().ascii_serialization())
        .ok();
    let origin = &head.verified.htu_origin;
    if own_origin.as_deref() != Some(origin.as_str())
        && !state.domain_cache.is_allowed_origin(origin).await
    {
        let r = Refusal::Proof("origin");
        log_refusal(&state, &ticket, r.step(), &method, path).await;
        return refuse(r);
    }

    let (mut parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => {
            tracing::warn!(step = "body_size", path, "refused a management ticket");
            log_refusal(&state, &ticket, "body_size", &method, path).await;
            return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response();
        }
    };
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    if let Err(r) = head.check_body(&digest) {
        log_refusal(&state, &ticket, r.step(), &method, path).await;
        return refuse(r);
    }
    match record_jti(
        &state.db,
        state.db_backend,
        &head.replay_key(),
        head.forget_at,
        now,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            log_refusal(&state, &ticket, Refusal::Replay.step(), &method, path).await;
            return refuse(Refusal::Replay);
        }
        Err(e) => return e.into_response(),
    }

    let verified = head.verified;
    parts.extensions.insert(verified.clone());
    let req = Request::from_parts(parts, Body::from(bytes));
    TICKET_REQUEST.scope(verified, next.run(req)).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::Signer;
    use serde_json::{Value, json};

    pub(crate) fn new_key() -> SigningKey {
        use rand::Rng;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        SigningKey::from_slice(&bytes).unwrap()
    }

    pub(crate) fn public_jwk(key: &SigningKey, kid: Option<&str>) -> Value {
        let point = key.verifying_key().to_sec1_point(false);
        let mut jwk = json!({
            "kty": "EC",
            "crv": "P-256",
            "x": URL_SAFE_NO_PAD.encode(point.x().unwrap()),
            "y": URL_SAFE_NO_PAD.encode(point.y().unwrap()),
        });
        if let Some(kid) = kid {
            jwk["kid"] = json!(kid);
        }
        jwk
    }

    pub(crate) fn sign(key: &SigningKey, header: &Value, claims: &Value) -> String {
        let h = URL_SAFE_NO_PAD.encode(serde_json::to_vec(header).unwrap());
        let c = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
        let input = format!("{h}.{c}");
        let sig: Signature = key.sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
    }

    struct Fixture {
        issuer_key: SigningKey,
        caller_key: SigningKey,
        trust: TicketTrust,
        now: u64,
    }

    const ISS: &str = "https://issuer.example";
    const AUD: &str = "hpy:instance:test";
    const DID: &str = "did:plc:owner";

    fn fixture() -> Fixture {
        let issuer_key = new_key();
        let caller_key = new_key();
        let jwks = json!({ "keys": [public_jwk(&issuer_key, Some("k1"))] }).to_string();
        let trust = TicketTrust::parse(Some(ISS), Some(AUD), Some(&jwks))
            .unwrap()
            .unwrap();
        Fixture {
            issuer_key,
            caller_key,
            trust,
            now: 1_800_000_000,
        }
    }

    impl Fixture {
        fn jkt(&self) -> String {
            crate::oauth::keys::compute_jwk_thumbprint(&public_jwk(&self.caller_key, None)).unwrap()
        }

        fn ticket_claims(&self) -> Value {
            json!({
                "iss": ISS, "sub": DID, "aud": AUD,
                "cnf": { "jkt": self.jkt() },
                "iat": self.now, "exp": self.now + 60,
                "jti": "ticket-1", "v": 1,
            })
        }

        fn ticket_with(&self, claims: Value) -> String {
            sign(
                &self.issuer_key,
                &json!({ "alg": "ES256", "typ": TICKET_TYP, "kid": "k1" }),
                &claims,
            )
        }

        fn ticket(&self) -> String {
            self.ticket_with(self.ticket_claims())
        }

        fn proof_claims(&self, ticket: &str, jti: &str) -> Value {
            json!({
                "htm": "PUT",
                "htu": "https://instance.example/admin/settings/x",
                "hq": b64_sha256(b"a=1"),
                "ct": "application/json",
                "bh": b64_sha256(b"{}"),
                "ath": b64_sha256(ticket.as_bytes()),
                "iat": self.now,
                "jti": jti,
            })
        }

        fn proof_with(&self, claims: Value) -> String {
            sign(
                &self.caller_key,
                &json!({ "alg": "ES256", "typ": PROOF_TYP, "jwk": public_jwk(&self.caller_key, None) }),
                &claims,
            )
        }

        fn facts(&self) -> RequestFacts<'static> {
            RequestFacts {
                method: "PUT",
                path: "/admin/settings/x",
                query: "a=1",
                content_type: Some("application/json"),
            }
        }

        fn check(&self, ticket: &str, proof: &str) -> Result<Verified, Refusal> {
            let head = verify_head(&self.trust, ticket, proof, &self.facts(), self.now)?;
            head.check_body(&Sha256::digest(b"{}").into())?;
            Ok(head.verified)
        }
    }

    fn unique(tag: &str) -> String {
        format!("{tag}-{}", uuid::Uuid::new_v4())
    }

    #[test]
    fn a_valid_ticket_and_proof_verify() {
        let f = fixture();
        let ticket = f.ticket();
        let proof = f.proof_with(f.proof_claims(&ticket, &unique("ok")));
        let v = f.check(&ticket, &proof).unwrap();
        assert_eq!(v.did, DID);
        assert_eq!(v.jkt, f.jkt());
        assert_eq!(v.ticket_jti, "ticket-1");
        assert_eq!(v.htu_origin, "https://instance.example");
    }

    #[test]
    fn the_ticket_must_come_from_a_pinned_key_for_this_instance() {
        let f = fixture();
        let other = new_key();
        let forged = sign(
            &other,
            &json!({ "alg": "ES256", "typ": TICKET_TYP, "kid": "k1" }),
            &f.ticket_claims(),
        );
        let proof = f.proof_with(f.proof_claims(&forged, &unique("forged")));
        assert_eq!(
            f.check(&forged, &proof).unwrap_err(),
            Refusal::Ticket("signature")
        );

        for (field, value, step) in [
            ("aud", json!("hpy:instance:other"), "audience"),
            ("iss", json!("https://elsewhere.example"), "issuer"),
            ("v", json!(2), "version"),
            ("sub", json!("not-a-did"), "subject"),
        ] {
            let mut claims = f.ticket_claims();
            claims[field] = value;
            let ticket = f.ticket_with(claims);
            let proof = f.proof_with(f.proof_claims(&ticket, &unique(step)));
            assert_eq!(f.check(&ticket, &proof).unwrap_err(), Refusal::Ticket(step));
        }
    }

    #[test]
    fn ticket_times_are_enforced() {
        let f = fixture();
        let mut claims = f.ticket_claims();
        claims["iat"] = json!(f.now - 200);
        claims["exp"] = json!(f.now - 140);
        let ticket = f.ticket_with(claims);
        let proof = f.proof_with(f.proof_claims(&ticket, &unique("exp")));
        assert_eq!(
            f.check(&ticket, &proof).unwrap_err(),
            Refusal::Ticket("expired")
        );

        for (exp, step) in [(f.now + 61, "lifetime"), (f.now + 3600, "lifetime")] {
            let mut claims = f.ticket_claims();
            claims["exp"] = json!(exp);
            let ticket = f.ticket_with(claims);
            let proof = f.proof_with(f.proof_claims(&ticket, &unique("long")));
            assert_eq!(f.check(&ticket, &proof).unwrap_err(), Refusal::Ticket(step));
        }

        // Sixty seconds is the most a ticket may claim, and 30 seconds of
        // leeway apply either side of it.
        let ticket = f.ticket();
        let proof = f.proof_with(f.proof_claims(&ticket, &unique("sixty")));
        assert!(f.check(&ticket, &proof).is_ok());

        let mut claims = f.ticket_claims();
        claims["iat"] = json!(f.now - 85);
        claims["exp"] = json!(f.now - 25);
        let ticket = f.ticket_with(claims);
        let proof = f.proof_with(f.proof_claims(&ticket, &unique("leeway")));
        assert!(f.check(&ticket, &proof).is_ok());

        let mut claims = f.ticket_claims();
        claims["iat"] = json!(f.now + 31);
        claims["exp"] = json!(f.now + 91);
        let ticket = f.ticket_with(claims);
        let proof = f.proof_with(f.proof_claims(&ticket, &unique("future")));
        assert_eq!(
            f.check(&ticket, &proof).unwrap_err(),
            Refusal::Ticket("not_yet_valid")
        );
    }

    #[tokio::test]
    async fn events_written_for_a_ticket_request_name_the_ticket() {
        let detail = json!({ "key": "app_name" });
        assert!(event_detail(&detail).is_none());

        let ticket = Verified {
            did: DID.into(),
            jkt: "jkt-1".into(),
            ticket_jti: "ticket-1".into(),
            issuer: ISS.into(),
            htu_origin: "https://instance.example".into(),
        };
        let stamped = scope(ticket, async { event_detail(&detail) })
            .await
            .unwrap();
        assert_eq!(
            stamped,
            json!({
                "key": "app_name",
                "auth": "ticket",
                "ticket": { "jti": "ticket-1", "iss": ISS, "jkt": "jkt-1" },
            })
        );

        assert_eq!(
            with_ticket_detail(&Value::Null, "j", "i", "k")["auth"],
            "ticket"
        );
        assert_eq!(
            with_ticket_detail(&json!([1]), "j", "i", "k")["value"],
            json!([1])
        );
    }

    #[test]
    fn the_proof_must_be_signed_by_the_ticket_key() {
        let f = fixture();
        let ticket = f.ticket();
        let stranger = new_key();
        let proof = sign(
            &stranger,
            &json!({ "alg": "ES256", "typ": PROOF_TYP, "jwk": public_jwk(&stranger, None) }),
            &f.proof_claims(&ticket, &unique("stranger")),
        );
        assert_eq!(
            f.check(&ticket, &proof).unwrap_err(),
            Refusal::Proof("key_mismatch")
        );
    }

    #[test]
    fn the_proof_binds_every_part_of_the_request() {
        let f = fixture();
        let ticket = f.ticket();
        for (field, value, step) in [
            ("htm", json!("DELETE"), "method"),
            (
                "htu",
                json!("https://instance.example/admin/settings/y"),
                "url",
            ),
            (
                "htu",
                json!("https://instance.example/admin/settings/x?a=1"),
                "url",
            ),
            ("hq", json!(b64_sha256(b"a=2")), "query"),
            ("ct", json!("text/plain"), "content_type"),
            ("bh", json!(b64_sha256(b"{\"x\":1}")), "body"),
            (
                "ath",
                json!(b64_sha256(b"another ticket")),
                "ticket_binding",
            ),
            ("iat", json!(f.now - 3600), "time"),
        ] {
            let mut claims = f.proof_claims(&ticket, &unique(step));
            claims[field] = value;
            let proof = f.proof_with(claims);
            assert_eq!(
                f.check(&ticket, &proof).unwrap_err(),
                Refusal::Proof(step),
                "{field}"
            );
        }

        let mut claims = f.proof_claims(&ticket, &unique("no-ct"));
        claims.as_object_mut().unwrap().remove("ct");
        let proof = f.proof_with(claims);
        assert_eq!(
            f.check(&ticket, &proof).unwrap_err(),
            Refusal::Proof("content_type")
        );
    }

    #[test]
    fn settings_must_be_complete_and_public() {
        assert!(TicketTrust::parse(None, None, None).unwrap().is_none());
        assert!(
            TicketTrust::parse(Some(" "), Some(""), None)
                .unwrap()
                .is_none()
        );
        assert!(TicketTrust::parse(Some(ISS), None, None).is_err());

        let key = new_key();
        let mut private = public_jwk(&key, Some("k1"));
        private["d"] = json!(URL_SAFE_NO_PAD.encode(key.to_bytes()));
        let jwks = json!({ "keys": [private] }).to_string();
        assert!(TicketTrust::parse(Some(ISS), Some(AUD), Some(&jwks)).is_err());

        let jwks = json!({ "keys": [public_jwk(&key, None)] }).to_string();
        assert!(TicketTrust::parse(Some(ISS), Some(AUD), Some(&jwks)).is_err());
        assert!(TicketTrust::parse(Some(ISS), Some(AUD), Some(r#"{"keys":[]}"#)).is_err());
    }

    #[test]
    fn a_ticket_request_carries_nothing_else() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "HV-Ticket x".parse().unwrap());
        assert!(check_alone(&headers).is_ok());

        for (name, value) in [
            ("cookie", "happyview_session=x"),
            ("dpop", "x"),
            ("x-client-key", "hvc_x"),
        ] {
            let mut h = headers.clone();
            h.insert(name, value.parse().unwrap());
            assert_eq!(
                check_alone(&h).unwrap_err(),
                Refusal::MixedCredentials,
                "{name}"
            );
        }
        let mut h = headers.clone();
        h.append("authorization", "Bearer hv_x".parse().unwrap());
        assert_eq!(check_alone(&h).unwrap_err(), Refusal::MixedCredentials);

        let mut h = headers.clone();
        h.insert("upgrade", "websocket".parse().unwrap());
        assert_eq!(check_alone(&h).unwrap_err(), Refusal::Upgrade);
    }
}
