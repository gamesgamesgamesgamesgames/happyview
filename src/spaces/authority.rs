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

const PLC_PREPARED: &str = "prepared";
const PLC_PUBLISHED: &str = "published";

/// Create and publish a DID for a space about to be created, returning it.
///
/// The keys and the signed genesis operation are stored before the operation
/// is submitted. A failed submission leaves the row `prepared` and fails the
/// call, so no space is created on a DID the directory does not know.
pub(crate) async fn create(
    state: &AppState,
    creator_did: &str,
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
    if publish && state.config.plc_url.is_empty() {
        return Err(AppError::ServerMisconfigured(
            "PLC_URL is required to create a space: each space gets its own did:plc".into(),
        ));
    }

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
         (did, signing_key_enc, kem_key_enc, rotation_key_enc, creator_rotation_key, genesis_op, plc_state, created_by, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to store a space's keys: {e}")))?;

    if !publish {
        return Ok(did);
    }
    crate::plc::submit_operation(&state.http, &state.config.plc_url, &did, &signed).await?;

    crate::db::query(&adapt_sql(
        "UPDATE happyview_space_authorities SET plc_state = ?, updated_at = ? WHERE did = ?",
        state.db_backend,
    ))
    .bind(PLC_PUBLISHED)
    .bind(now_rfc3339())
    .bind(&did)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to record a space DID as published: {e}")))?;

    Ok(did)
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
        "SELECT signing_key_enc FROM happyview_space_authorities WHERE did = ?",
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

/// Whether `did` is a space DID this instance holds.
pub(crate) async fn holds(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    did: &str,
) -> Result<bool, AppError> {
    let row: Option<(String,)> = crate::db::query_as(&adapt_sql(
        "SELECT did FROM happyview_space_authorities WHERE did = ?",
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
