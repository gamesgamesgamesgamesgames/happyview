//! A space's own did:plc.
//!
//! Proposal 0016 resolves a space authority like any account: its
//! `#atproto_pds` service is the space host, and its `#atproto` key signs the
//! space's credentials and any service auth sent as the authority. A space
//! anchored on a user's DID can therefore be hosted only by that user's PDS, so
//! every space this instance creates gets a DID of its own, whose
//! `#atproto_pds` names this instance.
//!
//! Its genesis operation lists HappyView's rotation key, and ahead of it any
//! rotation key the creator supplied. A key earlier in the list can nullify an
//! operation signed by a later one for 72 hours, so a creator holding one can
//! move the space to another host without this instance's cooperation, and
//! undo anything this instance signed.
//!
//! The DID also publishes `#atproto_kem`, because the authority has a space
//! repo of its own (for published policy and member records) and this instance
//! is its repo host.

use p256::ecdsa::SigningKey;
use rand::Rng;

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::plugin::encryption::Keyring;

/// The `kid` of everything signed with an authority's `#atproto` key.
pub const SIGNING_KID: &str = "#atproto";

/// Stored and signed, not yet accepted by the directory.
const PLC_PREPARED: &str = "prepared";
/// Accepted by the directory.
const PLC_PUBLISHED: &str = "published";
/// Created by a `happyview test` run, which has no directory to publish to.
const PLC_LOCAL: &str = "local";

/// Create and publish the DID for the space `creator_did` is about to create
/// with `space_type` and `skey`, returning it.
///
/// One DID belongs to each (creator, type, key). A DID left `prepared` by a
/// submission that failed is submitted again with its stored operation, and
/// the DID of a deleted space is reused, so a recreated space has the address
/// its members' records name. The keys and the signed genesis operation are
/// stored before the operation is submitted, and a failed submission fails the
/// call, so no space is created on a DID the directory does not know.
pub(crate) async fn create(
    state: &AppState,
    creator_did: &str,
    space_type: &str,
    skey: &str,
    creator_rotation_key: Option<&str>,
) -> Result<String, AppError> {
    let keyring = state.config.keyring().ok_or_else(|| {
        AppError::ServerMisconfigured(
            "TOKEN_ENCRYPTION_KEY is required to create a space: it seals the space's keys".into(),
        )
    })?;
    // A `happyview test` run has no PLC directory (its configuration blanks
    // PLC_URL), so there the DID is created and stored but not published.
    let publish = state.test.is_none();
    if publish {
        check_publishable(&state.config.plc_url, &state.config.public_url)?;
    }

    let existing: Option<(String, String, String, Option<String>)> = crate::db::query_as(&adapt_sql(
        "SELECT did, genesis_op, plc_state, creator_rotation_key FROM happyview_space_authorities \
         WHERE created_by = ? AND space_type = ? AND skey = ?",
        state.db_backend,
    ))
    .bind(creator_did)
    .bind(space_type)
    .bind(skey)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to look up a space DID: {e}")))?;

    let (did, signed, plc_state) = match existing {
        Some((did, genesis_op, plc_state, stored_rotation_key)) => {
            if creator_rotation_key.is_some()
                && creator_rotation_key != stored_rotation_key.as_deref()
            {
                return Err(AppError::Conflict(
                    "this space's DID already exists, with a different rotation key".into(),
                ));
            }
            let signed: serde_json::Value = serde_json::from_str(&genesis_op).map_err(|e| {
                AppError::Internal(format!("a stored genesis operation is unreadable: {e}"))
            })?;
            (did, signed, plc_state)
        }
        None => {
            let (did, signed) = prepare(
                state,
                &keyring,
                creator_did,
                space_type,
                skey,
                creator_rotation_key,
            )
            .await?;
            (did, signed, PLC_PREPARED.to_string())
        }
    };

    if plc_state == PLC_PUBLISHED || (!publish && plc_state == PLC_LOCAL) {
        return Ok(did);
    }
    if publish {
        crate::plc::submit_operation(&state.http, &state.config.plc_url, &did, &signed).await?;
    }
    crate::db::query(&adapt_sql(
        "UPDATE happyview_space_authorities SET plc_state = ?, updated_at = ? WHERE did = ?",
        state.db_backend,
    ))
    .bind(if publish { PLC_PUBLISHED } else { PLC_LOCAL })
    .bind(now_rfc3339())
    .bind(&did)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to record a space DID as published: {e}")))?;

    Ok(did)
}

/// Generate the keys and genesis operation for a new space DID, and store them
/// as `prepared`. A concurrent create of the same space loses on the table's
/// (creator, type, key) constraint, as a conflict.
async fn prepare(
    state: &AppState,
    keyring: &Keyring,
    creator_did: &str,
    space_type: &str,
    skey: &str,
    creator_rotation_key: Option<&str>,
) -> Result<(String, serde_json::Value), AppError> {
    let signing = random_key()?;
    let kem = random_key()?;
    let rotation = random_key()?;

    let mut rotation_keys: Vec<String> = creator_rotation_key
        .map(str::to_owned)
        .into_iter()
        .collect();
    rotation_keys.push(did_key(&rotation)?);
    let params = happyview_plc::PlcGenesisParams {
        rotation_keys,
        verification_methods: std::collections::BTreeMap::from([
            ("atproto".to_string(), did_key(&signing)?),
            ("atproto_kem".to_string(), did_key(&kem)?),
        ]),
        also_known_as: vec![],
        service_entries: vec![(
            "atproto_pds".into(),
            "AtprotoPersonalDataServer".into(),
            state.config.public_url.clone(),
        )],
    };
    // A creator key that is not a P-256 or secp256k1 did:key fails here, as a
    // bad request naming the key.
    let signed =
        happyview_plc::sign_operation(&happyview_plc::build_unsigned_genesis(&params), &rotation)?;
    let did = happyview_plc::derive_did(&signed)?;

    let seal = |key: &SigningKey| {
        crate::plugin::encryption::encrypt(keyring.current(), &key.to_bytes())
            .map_err(|e| AppError::Internal(format!("failed to seal a space key: {e}")))
    };
    let now = now_rfc3339();
    crate::db::query(&adapt_sql(
        "INSERT INTO happyview_space_authorities \
         (did, signing_key_enc, kem_key_enc, rotation_key_enc, creator_rotation_key, genesis_op, plc_state, created_by, space_type, skey, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        state.db_backend,
    ))
    .bind(&did)
    .bind(seal(&signing)?)
    .bind(seal(&kem)?)
    .bind(seal(&rotation)?)
    .bind(creator_rotation_key)
    .bind(signed.to_string())
    .bind(PLC_PREPARED)
    .bind(creator_did)
    .bind(space_type)
    .bind(skey)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| match e.as_database_error().map(|d| d.is_unique_violation()) {
        Some(true) => AppError::Conflict("You already have a space with this type and key".into()),
        _ => AppError::Internal(format!("failed to store a space's keys: {e}")),
    })?;

    Ok((did, signed))
}

/// Refuse to publish a DID to plc.directory that names an address only this
/// machine can reach. A did:plc is permanent, so a development instance would
/// leave one behind for every space it creates.
fn check_publishable(plc_url: &str, public_url: &str) -> Result<(), AppError> {
    if plc_url.is_empty() {
        return Err(AppError::ServerMisconfigured(
            "PLC_URL is required to create a space: each space gets its own did:plc".into(),
        ));
    }
    let public_directory = url::Url::parse(plc_url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.eq_ignore_ascii_case("plc.directory"))
        })
        .unwrap_or(false);
    if !public_directory {
        return Ok(());
    }
    let reachable = url::Url::parse(public_url).ok().is_some_and(|u| {
        u.scheme() == "https"
            && match u.host() {
                Some(url::Host::Domain(host)) => {
                    let host = host.to_ascii_lowercase();
                    host != "localhost" && !host.ends_with(".localhost")
                }
                Some(url::Host::Ipv4(ip)) => {
                    !(ip.is_loopback()
                        || ip.is_private()
                        || ip.is_link_local()
                        || ip.is_unspecified())
                }
                Some(url::Host::Ipv6(ip)) => !(ip.is_loopback() || ip.is_unspecified()),
                None => false,
            }
    });
    if reachable {
        Ok(())
    } else {
        Err(AppError::ServerMisconfigured(format!(
            "creating a space publishes a permanent did:plc naming PUBLIC_URL ({public_url}) as its host, \
             and plc.directory would keep one for every space this instance creates; \
             set PUBLIC_URL to the instance's public https address, or PLC_URL to a local PLC directory"
        )))
    }
}

/// The `#atproto` signing key of a space DID this instance holds, or `None`
/// when `did` is not one: a space created before space DIDs existed, anchored
/// on the instance's DID or its creator's.
pub(crate) async fn signing_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    keyring: &Keyring,
    did: &str,
) -> Result<Option<SigningKey>, AppError> {
    let row: Option<(Vec<u8>,)> = crate::db::query_as(&adapt_sql(
        "SELECT signing_key_enc FROM happyview_space_authorities WHERE did = ? AND plc_state IN ('published', 'local')",
        backend,
    ))
    .bind(did)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(format!("failed to look up a space's signing key: {e}")))?;
    let Some((sealed,)) = row else {
        return Ok(None);
    };
    let bytes = keyring
        .decrypt(&sealed)
        .map_err(|e| AppError::Internal(format!("failed to unseal a space's signing key: {e}")))?;
    SigningKey::from_slice(&bytes)
        .map(Some)
        .map_err(|e| AppError::Internal(format!("invalid space signing key: {e}")))
}

/// Whether `did` is a space DID this instance holds. A DID still `prepared`
/// names no space, so it does not count.
pub(crate) async fn holds(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    did: &str,
) -> Result<bool, AppError> {
    let row: Option<(String,)> = crate::db::query_as(&adapt_sql(
        "SELECT did FROM happyview_space_authorities WHERE did = ? AND plc_state IN ('published', 'local')",
        backend,
    ))
    .bind(did)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(format!("failed to look up a space DID: {e}")))?;
    Ok(row.is_some())
}

/// Service auth signed as a space's own DID, for a call the authority makes:
/// a managing-app check, a credential revocation, a deletion notice, a
/// forwarded write notification. `None` when `authority_did` is not a space
/// DID this instance holds; the caller then falls back to how it signs for a
/// space created before space DIDs existed.
pub(crate) async fn mint_service_auth(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    keyring: &Keyring,
    authority_did: &str,
    aud: &str,
    lxm: &str,
) -> Result<Option<String>, AppError> {
    match signing_key(pool, backend, keyring, authority_did).await? {
        Some(key) => {
            crate::auth::service_auth::sign_service_auth(&key, authority_did, aud, lxm).map(Some)
        }
        None => Ok(None),
    }
}

/// Service auth for the calls a space created before space DIDs existed signs
/// as this instance: deletion notices, forwarded write notifications,
/// revocations and webhook deliveries. A space on its own DID signs them as
/// that DID.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn mint_service_auth_or_instance(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    keyring: &Keyring,
    public_url: &str,
    authority_did: &str,
    aud: &str,
    lxm: &str,
) -> Result<String, AppError> {
    if let Some(token) = mint_service_auth(pool, backend, keyring, authority_did, aud, lxm).await? {
        return Ok(token);
    }
    crate::auth::service_auth::mint_service_auth(pool, backend, keyring, public_url, aud, lxm).await
}

fn random_key() -> Result<SigningKey, AppError> {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    SigningKey::from_slice(&bytes)
        .map_err(|e| AppError::Internal(format!("failed to generate a space key: {e}")))
}

fn did_key(key: &SigningKey) -> Result<String, AppError> {
    Ok(happyview_plc::private_key_to_did_key(&key.to_bytes())?)
}

#[cfg(test)]
mod tests {
    use super::check_publishable;

    /// The (creator, type, key) constraint is what settles two creates that
    /// both found no DID for the space: the second is a conflict.
    #[tokio::test]
    async fn a_second_did_for_one_space_is_a_conflict() {
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let keyring = state.config.keyring().unwrap();
        super::prepare(
            &state,
            &keyring,
            "did:plc:creator",
            "com.example.chat",
            "general",
            None,
        )
        .await
        .unwrap();
        let err = super::prepare(
            &state,
            &keyring,
            "did:plc:creator",
            "com.example.chat",
            "general",
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, crate::error::AppError::Conflict(_)),
            "{err:?}"
        );
    }

    #[test]
    fn plc_directory_needs_a_public_https_address() {
        for public_url in [
            "http://127.0.0.1:3000",
            "http://localhost:3000",
            "https://localhost",
            "https://happyview.localhost",
            "https://10.0.0.5",
            "https://[::1]",
            "http://hv.example.com",
        ] {
            assert!(
                check_publishable("https://plc.directory", public_url).is_err(),
                "{public_url}"
            );
        }
        check_publishable("https://plc.directory", "https://hv.example.com").unwrap();
    }

    #[test]
    fn another_directory_takes_any_address() {
        check_publishable("http://127.0.0.1:2582", "http://127.0.0.1:0").unwrap();
        assert!(check_publishable("", "https://hv.example.com").is_err());
    }
}
