//! A real refresh of a linked repo's OAuth session.
//!
//! atrium-oauth 0.1.7 refreshes a session only inside a request that the PDS
//! answers `invalid_token`, and drops the refresh's error. `restore` reads the
//! stored session and the authorization server's metadata, and never calls
//! the token endpoint. So the keep-alive refreshes here: one refresh grant at
//! the session's authorization server, the new tokens written back to the
//! session atrium reads.
//!
//! A refresh token used twice is a replay, and the reference provider revokes
//! the whole grant for it. atrium's refresh and this one share no lock, so
//! this one first takes a lease through the session row itself. One
//! compare-and-set moves the stored `expires_at` [`LEASE`] ahead, which atrium
//! reads as a live token and so doesn't refresh, and marks the row
//! `refresh_lease_until`, which another keep-alive reads as taken. The new
//! tokens replace the leased row; a failure puts the row back as it was. What
//! a lease can't stop is a refresh atrium had already started, from a row it
//! read before the lease was taken.

use atrium_api::types::string::Did;

use crate::AppState;
use crate::HappyViewOAuthClient;
use crate::auth::oauth_store::{DbSessionStore, StoredJson};

use super::client::LINKED_SESSIONS_TABLE;
use super::refresh::{self, RefreshFailure};

/// How long a refresh holds the session.
const LEASE: chrono::Duration = chrono::Duration::minutes(2);
/// The field a held session carries, beside atrium's own.
const LEASE_FIELD: &str = "refresh_lease_until";

/// What a refresh did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refreshed {
    /// New tokens are stored.
    Done,
    /// Another refresh holds the session; it settles the grant.
    InProgress,
}

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

fn store_error(e: impl std::fmt::Display) -> RefreshError {
    RefreshError::classified(format!("session store error: {e}"))
}

fn rfc3339(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Whether `session` is held by a refresh whose lease hasn't run out.
fn leased(session: &serde_json::Value, now: chrono::DateTime<chrono::Utc>) -> bool {
    session[LEASE_FIELD]
        .as_str()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|until| until > now)
}

/// Refresh `did`'s linked-repo session as `client`, and store the new tokens.
pub async fn refresh_session(
    state: &AppState,
    client: &HappyViewOAuthClient,
    did: &str,
) -> Result<Refreshed, RefreshError> {
    Did::new(did.to_string())
        .map_err(|_| RefreshError::local(format!("invalid DID on grant: {did}")))?;
    let store = session_store(state);
    let original = store
        .read_json(did)
        .await
        .map_err(store_error)?
        .ok_or_else(|| RefreshError::classified("session does not exist".into()))?;
    let session: serde_json::Value = serde_json::from_str(&original.json)
        .map_err(|e| RefreshError::classified(format!("session store error: json error: {e}")))?;

    let now = chrono::Utc::now();
    if leased(&session, now) {
        return Ok(Refreshed::InProgress);
    }
    let refresh_token = session["token_set"]["refresh_token"]
        .as_str()
        .ok_or_else(|| RefreshError::classified("no refresh token available".into()))?
        .to_string();
    let issuer = session["token_set"]["iss"]
        .as_str()
        .ok_or_else(|| RefreshError::local("stored session names no issuer".into()))?
        .to_string();

    // Settled before the lease, so nothing that can fail early holds it.
    let client_id = client.client_metadata.client_id.clone();
    let confidential =
        client.client_metadata.token_endpoint_auth_method.as_deref() == Some("private_key_jwt");
    let signing = if confidential {
        Some(signing_key(state, did).await?)
    } else {
        None
    };

    let mut lease = session.clone();
    lease[LEASE_FIELD] = rfc3339(now + LEASE).into();
    lease["token_set"]["expires_at"] = rfc3339(now + LEASE).into();
    let Some(held) = store
        .compare_and_set_json(did, &original, lease.to_string())
        .await
        .map_err(store_error)?
    else {
        // The row changed since it was read: another refresh got there first.
        return Ok(Refreshed::InProgress);
    };

    let outcome = refresh_and_store(
        state,
        &store,
        did,
        &session,
        &held,
        &issuer,
        &refresh_token,
        &client_id,
        signing.as_ref(),
    )
    .await;
    if outcome.is_err() {
        // Put the session back as it was, so atrium can refresh it.
        if let Err(e) = store
            .compare_and_set_json(did, &held, original.json.clone())
            .await
        {
            tracing::warn!(did, error = %e, "failed to release a linked repo session lease");
        }
    }
    outcome.map(|()| Refreshed::Done)
}

#[allow(clippy::too_many_arguments)]
async fn refresh_and_store(
    state: &AppState,
    store: &DbSessionStore,
    did: &str,
    session: &serde_json::Value,
    held: &StoredJson,
    issuer: &str,
    refresh_token: &str,
    client_id: &str,
    signing: Option<&crate::oauth::client_keys::ClientKey>,
) -> Result<(), RefreshError> {
    let dpop_jwk = &session["dpop_key"];
    let tokens = crate::oauth::pds_write::refresh_token_set(
        &state.http,
        issuer,
        refresh_token,
        client_id,
        dpop_jwk,
        signing.map(|k| (&k.private_jwk, k.kid.as_str())),
    )
    .await
    .map_err(|e| RefreshError::classified(e.to_string()))?;

    let refused = |message: String| RefreshError {
        message,
        kind: RefreshFailure::Refused,
    };
    match tokens.sub.as_deref() {
        Some(sub) if sub == did => {}
        Some(sub) => {
            return Err(refused(format!(
                "token refresh answered for {sub}, not {did}"
            )));
        }
        None => return Err(refused("token refresh answered with no sub".into())),
    }
    if !tokens
        .token_type
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case("DPoP"))
    {
        return Err(refused(format!(
            "token refresh answered a {} token, not DPoP",
            tokens.token_type.as_deref().unwrap_or("untyped")
        )));
    }

    let mut updated = session.clone();
    if let Some(fields) = updated.as_object_mut() {
        fields.remove(LEASE_FIELD);
    }
    let token_set = &mut updated["token_set"];
    token_set["access_token"] = tokens.access_token.into();
    if let Some(rotated) = tokens.refresh_token {
        token_set["refresh_token"] = rotated.into();
    }
    if let Some(scope) = tokens.scope {
        token_set["scope"] = scope.into();
    }
    token_set["expires_at"] = match tokens.expires_in {
        Some(secs) => rfc3339(chrono::Utc::now() + chrono::Duration::seconds(secs)).into(),
        None => serde_json::Value::Null,
    };
    // atrium has to read what's written back.
    serde_json::from_value::<atrium_oauth::store::session::Session>(updated.clone())
        .map_err(|e| RefreshError::local(format!("unusable refreshed session: {e}")))?;

    match store
        .compare_and_set_json(did, held, updated.to_string())
        .await
        .map_err(store_error)?
    {
        Some(_) => Ok(()),
        None => Err(RefreshError::local(
            "the session changed while its refresh held it".into(),
        )),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lease_holds_until_it_runs_out() {
        let now = chrono::Utc::now();
        let held = |until: chrono::DateTime<chrono::Utc>| serde_json::json!({ LEASE_FIELD: rfc3339(until) });
        assert!(leased(&held(now + chrono::Duration::seconds(30)), now));
        assert!(!leased(&held(now - chrono::Duration::seconds(1)), now));
        assert!(!leased(&serde_json::json!({}), now));
    }
}
