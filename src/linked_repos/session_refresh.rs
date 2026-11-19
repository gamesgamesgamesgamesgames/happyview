//! A real refresh of a linked repo's OAuth session.
//!
//! atrium-oauth 0.1.7 refreshes a session only inside a request that the PDS
//! answers `invalid_token`, and drops the refresh's error. `restore` reads the
//! stored session and the authorization server's metadata, and never calls
//! the token endpoint. So the keep-alive refreshes here: one refresh grant at
//! the session's authorization server, the new tokens written back to the
//! session atrium reads.

use atrium_api::types::string::Did;
use atrium_common::store::Store;

use crate::AppState;
use crate::HappyViewOAuthClient;
use crate::auth::oauth_store::DbSessionStore;

use super::client::LINKED_SESSIONS_TABLE;
use super::refresh::{self, RefreshFailure};

/// Why a refresh failed, and what that means for the grant.
#[derive(Debug)]
pub struct RefreshError {
    pub message: String,
    pub kind: RefreshFailure,
}

impl RefreshError {
    fn classified(message: String) -> Self {
        let kind = refresh::classify(&message);
        Self { message, kind }
    }

    fn local(message: String) -> Self {
        Self {
            message,
            kind: RefreshFailure::Local,
        }
    }
}

fn session_store(state: &AppState) -> DbSessionStore {
    DbSessionStore::new_with_table(
        state.db.clone(),
        state.db_backend,
        LINKED_SESSIONS_TABLE,
        state.config.keyring(),
    )
}

/// Refresh `did`'s linked-repo session as `client`, and store the new tokens.
///
/// A refusal that comes after another refresh rotated the session's refresh
/// token, such as one atrium made inside a request meanwhile, is that other
/// refresh's success, not a refusal of the grant.
pub async fn refresh_session(
    state: &AppState,
    client: &HappyViewOAuthClient,
    did: &str,
) -> Result<(), RefreshError> {
    let store = session_store(state);
    let key = Did::new(did.to_string())
        .map_err(|_| RefreshError::local(format!("invalid DID on grant: {did}")))?;
    let mut session = store
        .get(&key)
        .await
        .map_err(|e| RefreshError::classified(format!("session store error: {e}")))?
        .ok_or_else(|| RefreshError::classified("session does not exist".into()))?;
    let refresh_token = session
        .token_set
        .refresh_token
        .clone()
        .ok_or_else(|| RefreshError::classified("no refresh token available".into()))?;
    let dpop_jwk = serde_json::to_value(&session.dpop_key)
        .map_err(|e| RefreshError::local(format!("unreadable DPoP key: {e}")))?;

    let client_id = client.client_metadata.client_id.clone();
    let signing = if crate::auth::client_registry::is_loopback_url(&state.config.public_url) {
        None
    } else {
        Some(signing_key(state, did).await?)
    };

    let refreshed = crate::oauth::pds_write::refresh_token_set(
        &state.http,
        &session.token_set.iss,
        &refresh_token,
        &client_id,
        &dpop_jwk,
        signing.as_ref().map(|k| (&k.private_jwk, k.kid.as_str())),
    )
    .await;

    let tokens = match refreshed {
        Ok(tokens) => tokens,
        Err(e) => {
            let err = RefreshError::classified(e.to_string());
            if err.kind == RefreshFailure::Refused
                && rotated_since(&store, &key, &refresh_token).await
            {
                return Ok(());
            }
            return Err(err);
        }
    };

    if tokens.sub.as_deref().is_some_and(|sub| sub != did) {
        return Err(RefreshError {
            message: format!("token refresh answered for another account than {did}"),
            kind: RefreshFailure::Refused,
        });
    }

    let mut token_set = serde_json::to_value(&session.token_set)
        .map_err(|e| RefreshError::local(format!("unreadable token set: {e}")))?;
    token_set["access_token"] = tokens.access_token.into();
    if let Some(rotated) = tokens.refresh_token {
        token_set["refresh_token"] = rotated.into();
    }
    if let Some(scope) = tokens.scope {
        token_set["scope"] = scope.into();
    }
    token_set["expires_at"] = match tokens.expires_in {
        Some(secs) => (chrono::Utc::now() + chrono::Duration::seconds(secs))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .into(),
        None => serde_json::Value::Null,
    };
    session.token_set = serde_json::from_value(token_set)
        .map_err(|e| RefreshError::local(format!("unusable refreshed token set: {e}")))?;

    store
        .set(key, session)
        .await
        .map_err(|e| RefreshError::classified(format!("session store error: {e}")))
}

/// The instance key that signs this session's client assertions: the one it
/// was made with, or the current key for a session that names none.
async fn signing_key(
    state: &AppState,
    did: &str,
) -> Result<crate::oauth::client_keys::ClientKey, RefreshError> {
    let kid = crate::auth::oauth_store::lookup_signing_kid(
        &state.db,
        state.db_backend,
        LINKED_SESSIONS_TABLE,
        did,
    )
    .await
    .map_err(|e| RefreshError::local(format!("session store error: {e}")))?;
    let keys = crate::oauth::client_keys::load_keys(
        &state.db,
        state.db_backend,
        state.config.keyring().as_ref(),
        crate::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .map_err(|e| RefreshError::local(e.to_string()))?;
    match crate::oauth::client_keys::resolve_signing_key(&keys, kid.as_deref()) {
        Ok(Some(key)) => Ok(key.clone()),
        // The key the session was made with is gone: only a new authorization
        // can replace it.
        Ok(None) | Err(_) => Err(RefreshError {
            message: "the key this session was established with is no longer available".into(),
            kind: RefreshFailure::Refused,
        }),
    }
}

/// Whether the stored session's refresh token is no longer `used`.
async fn rotated_since(store: &DbSessionStore, key: &Did, used: &str) -> bool {
    matches!(
        store.get(key).await,
        Ok(Some(current)) if current.token_set.refresh_token.as_deref() != Some(used)
    )
}
