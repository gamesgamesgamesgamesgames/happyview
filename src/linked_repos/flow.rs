use std::sync::Arc;

use atrium_oauth::{AuthorizeOptions, Scope};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine;
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::HappyViewOAuthClient;
use crate::auth::client_registry::is_loopback_url;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;

use super::db;
use super::types::LinkedRepo;

/// How long an invite link stays valid.
const INVITE_TTL_SECS: i64 = 60 * 60 * 24 * 7;
/// How long an in-flight OAuth state row stays valid.
const STATE_TTL_SECS: i64 = 60 * 15;

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn expires_at(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339()
}

pub use crate::identity::ResolvedIdentifier;

pub async fn resolve_identifier(
    _state: &AppState,
    input: &str,
) -> Result<ResolvedIdentifier, AppError> {
    crate::identity::resolve_identifier(input).await
}

pub fn client_for_grant(
    state: &AppState,
    grant: &LinkedRepo,
) -> Result<Arc<HappyViewOAuthClient>, AppError> {
    if !is_loopback_url(&state.config.public_url) {
        return Ok(Arc::clone(&state.linked_repos_client));
    }

    let scopes: Vec<Scope> = crate::auth::parse_scope_string(&grant.scopes);
    let public_url = state.config.effective_public_url();
    let base = public_url.trim_end_matches('/');

    super::client::build(
        &state.config.plc_url,
        &format!("{base}/oauth-client-metadata.json"),
        &public_url,
        format!("{base}/auth/callback"),
        true,
        scopes,
        state.oauth_state_store.clone(),
        state.db.clone(),
        state.db_backend,
        None,
    )
    .map(Arc::new)
    .map_err(AppError::Internal)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthOrigin {
    Admin,
    Invite,
}

impl AuthOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Invite => "invite",
        }
    }

    pub fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("invite") => Self::Invite,
            _ => Self::Admin,
        }
    }
}

pub async fn start_authorization(
    state: &AppState,
    grant: &LinkedRepo,
    identifier: &str,
    origin: AuthOrigin,
) -> Result<String, AppError> {
    let scopes = crate::auth::parse_scope_string(&grant.scopes);
    let client = client_for_grant(state, grant)?;

    let guard = state.oauth_state_store.authorize_lock.lock().await;

    let url = client
        .authorize(
            identifier,
            AuthorizeOptions {
                scopes,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            let msg = format!("{e}");
            if msg.contains("invalid_scope") {
                AppError::BadRequest(format!(
                    "the authorization server rejected the requested scopes ({msg}). \
                     Either it does not accept one of this grant's scopes — in which \
                     case the grant must be recreated with scopes it does accept — or \
                     it is serving a cached copy of our client metadata, which a retry \
                     in a minute will clear."
                ))
            } else {
                AppError::Internal(format!("authorize failed: {msg}"))
            }
        })?;

    let oauth_state = state
        .oauth_state_store
        .take_last_state_key()
        .ok_or_else(|| AppError::Internal("no OAuth state captured for linked repo".into()))?;

    drop(guard);

    let sql = adapt_sql(
        "INSERT INTO happyview_linked_repo_auth_state (state, grant_id, expires_at, origin) \
         VALUES (?, ?, ?, ?)",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(&oauth_state)
        .bind(&grant.id)
        .bind(expires_at(STATE_TTL_SECS))
        .bind(origin.as_str())
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to store linked auth state: {e}")))?;

    Ok(url.to_string())
}

pub struct MintedInvite {
    pub token: String,
    pub expires_at: String,
}

pub const MIN_INVITE_TTL_SECS: i64 = 60;
pub const MAX_INVITE_TTL_SECS: i64 = 60 * 60 * 24 * 30;

pub async fn mint_invite_with_expiry(
    state: &AppState,
    grant_id: &str,
    ttl_secs: Option<i64>,
) -> Result<MintedInvite, AppError> {
    let ttl = match ttl_secs {
        Some(t) if !(MIN_INVITE_TTL_SECS..=MAX_INVITE_TTL_SECS).contains(&t) => {
            return Err(AppError::BadRequest(format!(
                "expires_in must be between {MIN_INVITE_TTL_SECS} and {MAX_INVITE_TTL_SECS} seconds"
            )));
        }
        Some(t) => t,
        None => INVITE_TTL_SECS,
    };

    let token = random_token();
    let hash = hash_token(&token);
    let expires = expires_at(ttl);

    let sql = adapt_sql(
        "INSERT INTO happyview_linked_repo_auth_state (state, grant_id, token_hash, expires_at) \
         VALUES (?, ?, ?, ?)",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(format!("invite:{hash}"))
        .bind(grant_id)
        .bind(&hash)
        .bind(&expires)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to store invite: {e}")))?;

    Ok(MintedInvite {
        token,
        expires_at: expires,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InviteSummary {
    pub invite_id: String,
    pub expires_at: String,
}

pub async fn list_invites(
    state: &AppState,
    grant_id: &str,
) -> Result<Vec<InviteSummary>, AppError> {
    let sql = adapt_sql(
        "SELECT token_hash, expires_at FROM happyview_linked_repo_auth_state \
         WHERE grant_id = ? AND token_hash IS NOT NULL AND expires_at > ? \
         ORDER BY expires_at",
        state.db_backend,
    );
    let rows: Vec<(String, String)> = crate::db::query_as(&sql)
        .bind(grant_id)
        .bind(now_rfc3339())
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list invites: {e}")))?;

    Ok(rows
        .into_iter()
        .map(|(invite_id, expires_at)| InviteSummary {
            invite_id,
            expires_at,
        })
        .collect())
}

pub async fn revoke_invite(
    state: &AppState,
    grant_id: &str,
    invite_id: &str,
) -> Result<bool, AppError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_linked_repo_auth_state \
         WHERE grant_id = ? AND token_hash = ? AND token_hash IS NOT NULL",
        state.db_backend,
    );
    let result = crate::db::query(&sql)
        .bind(grant_id)
        .bind(invite_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke invite: {e}")))?;

    Ok(result.rows_affected() > 0)
}

pub async fn invite_exists(state: &AppState, token: &str) -> Result<bool, AppError> {
    let hash = hash_token(token);
    let sql = adapt_sql(
        "SELECT 1 FROM happyview_linked_repo_auth_state \
         WHERE token_hash = ? AND expires_at > ?",
        state.db_backend,
    );
    let row: Option<(i32,)> = crate::db::query_as(&sql)
        .bind(&hash)
        .bind(now_rfc3339())
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up invite: {e}")))?;
    Ok(row.is_some())
}

pub async fn invite_grant_id(state: &AppState, token: &str) -> Result<Option<String>, AppError> {
    let hash = hash_token(token);
    let sql = adapt_sql(
        "SELECT grant_id FROM happyview_linked_repo_auth_state \
         WHERE token_hash = ? AND expires_at > ?",
        state.db_backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(&hash)
        .bind(now_rfc3339())
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up invite: {e}")))?;
    Ok(row.map(|(grant_id,)| grant_id))
}

pub async fn invalidate_invites_for_grant(
    state: &AppState,
    grant_id: &str,
) -> Result<u64, AppError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_linked_repo_auth_state \
         WHERE grant_id = ? AND token_hash IS NOT NULL",
        state.db_backend,
    );
    let result = crate::db::query(&sql)
        .bind(grant_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to invalidate invites: {e}")))?;
    Ok(result.rows_affected())
}

#[derive(Debug, PartialEq, Eq)]
pub enum PendingGrant {
    NotLinked,
    Expired,
    Grant {
        grant_id: String,
        origin: AuthOrigin,
    },
}

pub async fn take_pending_grant(
    state: &AppState,
    oauth_state: &str,
) -> Result<PendingGrant, AppError> {
    let sql = adapt_sql(
        "SELECT grant_id, expires_at, origin FROM happyview_linked_repo_auth_state \
         WHERE state = ? AND token_hash IS NULL",
        state.db_backend,
    );
    let row: Option<(String, String, Option<String>)> = crate::db::query_as(&sql)
        .bind(oauth_state)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up linked auth state: {e}")))?;

    let Some((grant_id, expires, origin)) = row else {
        return Ok(PendingGrant::NotLinked);
    };
    let origin = AuthOrigin::parse(origin.as_deref());

    let del = adapt_sql(
        "DELETE FROM happyview_linked_repo_auth_state WHERE state = ? AND token_hash IS NULL",
        state.db_backend,
    );
    let result = crate::db::query(&del)
        .bind(oauth_state)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to clear linked auth state: {e}")))?;

    if result.rows_affected() == 0 {
        return Ok(PendingGrant::Expired);
    }

    if expires.as_str() <= now_rfc3339().as_str() {
        return Ok(PendingGrant::Expired);
    }

    Ok(PendingGrant::Grant { grant_id, origin })
}

pub async fn complete(state: &AppState, grant_id: &str, did: &str) -> Result<(), AppError> {
    let grant = db::get(state, grant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("linked repo not found".into()))?;

    if let Some(ref expected) = grant.did
        && expected != did
    {
        tracing::warn!(
            grant_id = %grant.id,
            expected = %expected,
            authorized_by = %did,
            "refusing linked-repo completion: pinned grant authorized by a different account"
        );
        flag_session_lost(
            state,
            did,
            "another account's authorization overwrote this session",
        )
        .await;
        return Err(AppError::BadRequest(
            "this link was issued for a different account".into(),
        ));
    }

    if grant.did.is_none()
        && let Some(existing) = db::get_by_did(state, did).await?
        && existing.id != grant.id
    {
        tracing::warn!(
            grant_id = %grant.id,
            existing_grant_id = %existing.id,
            %did,
            "refusing linked-repo completion: DID already linked to another grant"
        );
        flag_session_lost(
            state,
            did,
            "another authorization for this account overwrote this session",
        )
        .await;
        return Err(AppError::Conflict(
            "this account is already linked to this instance".into(),
        ));
    }

    db::bind_did(state, grant_id, did, None).await
}

async fn flag_session_lost(state: &AppState, did: &str, reason: &str) {
    let existing = match db::get_by_did(state, did).await {
        Ok(Some(grant)) => grant,
        Ok(None) => return,
        Err(e) => {
            tracing::error!(%did, error = %e, "failed to look up grant for a refused authorization");
            return;
        }
    };
    if let Err(e) = db::mark_needs_reauth(state, &existing.id, reason).await {
        tracing::error!(grant_id = %existing.id, error = %e, "failed to flag grant as needing reauth");
    }
}

pub async fn discard_session(state: &AppState, did: &str) -> Result<(), AppError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_linked_repo_sessions WHERE did = ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(did)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to discard linked session: {e}")))?;
    Ok(())
}

#[derive(Deserialize)]
pub struct StartQuery {
    pub token: String,
    pub handle: Option<String>,
}

#[derive(serde::Serialize)]
pub struct InviteInfo {
    pub valid: bool,
    pub app_name: String,
    pub logo_url: Option<String>,
    pub scopes: Vec<String>,
    pub reason: Option<String>,
    pub pinned_identifier: Option<String>,
    pub expires_at: Option<String>,
}

fn invalid_invite_info() -> InviteInfo {
    InviteInfo {
        valid: false,
        app_name: String::new(),
        logo_url: None,
        scopes: Vec::new(),
        reason: None,
        pinned_identifier: None,
        expires_at: None,
    }
}

pub async fn invite_info_handler(
    State(state): State<AppState>,
    Query(query): Query<InviteInfoQuery>,
) -> Result<axum::Json<InviteInfo>, AppError> {
    let hash = hash_token(&query.token);
    let sql = adapt_sql(
        "SELECT grant_id, expires_at FROM happyview_linked_repo_auth_state \
         WHERE token_hash = ? AND expires_at > ?",
        state.db_backend,
    );
    let row: Option<(String, String)> = crate::db::query_as(&sql)
        .bind(&hash)
        .bind(now_rfc3339())
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up invite: {e}")))?;

    let Some((grant_id, expires_at)) = row else {
        return Ok(axum::Json(invalid_invite_info()));
    };

    let Some(grant) = db::get(&state, &grant_id).await? else {
        // The grant was deleted from a live invite
        return Ok(axum::Json(invalid_invite_info()));
    };

    let app_name = crate::admin::settings::get_setting(&state.db, "app_name", state.db_backend)
        .await
        .unwrap_or_else(|| "HappyView".to_string());

    let logo_url = crate::admin::settings::get_setting(&state.db, "logo_data", state.db_backend)
        .await
        .map(|_| {
            format!(
                "{}/settings/logo",
                state.config.effective_public_url().trim_end_matches('/')
            )
        })
        .or(crate::admin::settings::get_setting(&state.db, "logo_uri", state.db_backend).await);

    Ok(axum::Json(InviteInfo {
        valid: true,
        app_name,
        logo_url,
        scopes: super::scope::parse(&grant.scopes),
        reason: grant.reason.clone(),
        pinned_identifier: grant.handle.clone().or_else(|| grant.did.clone()),
        expires_at: Some(expires_at),
    }))
}

#[derive(Deserialize)]
pub struct InviteInfoQuery {
    pub token: String,
}

fn link_page(state: &AppState, path: &str) -> String {
    format!(
        "{}/link/{path}",
        state.config.effective_public_url().trim_end_matches('/')
    )
}

pub fn link_result_redirect(state: &AppState, status: &str, handle: Option<&str>) -> Response {
    let mut url = format!(
        "{}?status={}",
        link_page(state, "result"),
        urlencoding::encode(status)
    );
    if let Some(handle) = handle {
        url.push_str(&format!("&handle={}", urlencoding::encode(handle)));
    }
    Redirect::to(&url).into_response()
}

pub async fn start_handler(
    State(state): State<AppState>,
    Query(query): Query<StartQuery>,
) -> Result<Response, AppError> {
    let Some(handle) = query
        .handle
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(Redirect::to(&format!(
            "{}?token={}",
            link_page(&state, "start"),
            urlencoding::encode(&query.token)
        ))
        .into_response());
    };

    if !invite_exists(&state, &query.token).await? {
        return Err(AppError::BadRequest(
            "this link is invalid, expired, or has already been used".into(),
        ));
    }

    let resolved = resolve_identifier(&state, handle).await?;
    let identifier = resolved.handle.as_deref().unwrap_or(&resolved.did);

    let Some(grant_id) = invite_grant_id(&state, &query.token).await? else {
        return Err(AppError::BadRequest(
            "this link is invalid, expired, or has already been used".into(),
        ));
    };

    let grant = match db::get(&state, &grant_id).await {
        Ok(Some(grant)) => grant,
        Ok(None) => return Err(AppError::NotFound("linked repo not found".into())),
        Err(e) => return Err(e),
    };

    let url = start_authorization(&state, &grant, identifier, AuthOrigin::Invite).await?;
    Ok(Redirect::to(&url).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_token_is_stable_and_hex() {
        let a = hash_token("hello");
        assert_eq!(a, hash_token("hello"));
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, hash_token("hellp"));
    }

    #[test]
    fn random_token_is_url_safe_and_unique() {
        let a = random_token();
        let b = random_token();
        assert_ne!(a, b);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "token must be URL-safe, got {a}"
        );
    }
}
