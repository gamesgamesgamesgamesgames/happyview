use crate::AppState;
use crate::auth::COOKIE_NAME;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use atrium_oauth::{AuthorizeOptions, KnownScope, Scope};
use axum::{
    Json, Router,
    extract::{Query, State},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, Key, SignedCookieJar};
use serde::Deserialize;

/// Legacy cookie name from the old cookie-based redirect approach.
/// Detected and removed in the callback to clean up stale cookies.
const LEGACY_REDIRECT_COOKIE: &str = "happyview_redirect";

fn is_https(public_url: &str) -> bool {
    public_url.starts_with("https://")
}

#[derive(Deserialize)]
pub struct LoginQuery {
    handle: String,
    redirect_uri: Option<String>,
    scope: Option<String>,
    client_id: Option<String>,
}

/// Parse a whitespace-separated OAuth scope string into typed `Scope` values.
/// Known ATProto scope names are mapped to `Scope::Known`; anything else
/// (e.g. `include:*` permission set references) becomes `Scope::Unknown`.
pub fn parse_scope_string(scope_str: &str) -> Vec<Scope> {
    scope_str
        .split_whitespace()
        .map(|s| match s {
            "atproto" => Scope::Known(KnownScope::Atproto),
            "transition:generic" => Scope::Known(KnownScope::TransitionGeneric),
            "transition:chat.bsky" => Scope::Known(KnownScope::TransitionChatBsky),
            other => Scope::Unknown(other.to_string()),
        })
        .collect()
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: String,
    state: Option<String>,
    iss: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/login", get(login))
        .route("/callback", get(callback))
        .route("/logout", post(logout))
        .route("/me", get(me))
}

async fn login(
    State(state): State<AppState>,
    jar: SignedCookieJar<Key>,
    domain: Option<axum::extract::Extension<std::sync::Arc<crate::domain::Domain>>>,
    Query(query): Query<LoginQuery>,
) -> Result<(SignedCookieJar<Key>, Json<serde_json::Value>), AppError> {
    // Refuse to start a login flow we cannot finish securely: the session cookie
    // set by the callback is signed with the SESSION_SECRET-derived key.
    if !state.config.session_secret_secure() {
        return Err(AppError::ServerMisconfigured(
            crate::auth::COOKIE_AUTH_DISABLED_MSG.into(),
        ));
    }

    tracing::debug!(handle = %query.handle, redirect_uri = ?query.redirect_uri, scope = ?query.scope, "login request");

    // Use scopes from the query param if provided, otherwise fall back to the
    // settings DB. The client metadata advertises all possible scopes, but each
    // login request can ask for a subset.
    let scopes = if let Some(ref scope_str) = query.scope {
        let parsed = parse_scope_string(scope_str);
        if parsed.is_empty() {
            vec![Scope::Known(KnownScope::Atproto)]
        } else {
            parsed
        }
    } else {
        vec![Scope::Known(KnownScope::Atproto)]
    };

    tracing::debug!(scopes = ?scopes, client_id = ?query.client_id, "resolved oauth scopes");

    // For dashboard logins (no explicit client_id), use the domain's OAuth client
    let domain_url = domain.map(|d| d.0.url.clone());
    let effective_client_id = if query.client_id.is_some() {
        query.client_id.clone()
    } else {
        domain_url
            .as_ref()
            .map(|du| format!("{}/oauth-client-metadata.json", du.trim_end_matches('/')))
    };

    // Select the appropriate OAuth client based on client_id
    let oauth_client = state.oauth.get_or_default(effective_client_id.as_deref());

    // Hold the authorize lock so that authorize() + take_last_state_key() are atomic.
    // This prevents concurrent logins from swapping each other's state keys.
    let _authorize_guard = state.oauth_state_store.authorize_lock.lock().await;

    let options = AuthorizeOptions {
        scopes,
        ..Default::default()
    };

    let url = oauth_client
        .authorize(&query.handle, options)
        .await
        .map_err(|e| AppError::Internal(format!("OAuth authorize failed: {e}")))?;

    // Capture the state key immediately after authorize(). We can't parse it from the URL
    // because atrium uses PAR (Pushed Authorization Requests), so the state is embedded
    // in the pushed request, not visible in the URL.
    let oauth_state = state.oauth_state_store.take_last_state_key();

    drop(_authorize_guard);

    tracing::debug!(authorize_url = %url, "authorize URL generated");

    // Store the redirect URI in the database, keyed by the OAuth state parameter.
    // This avoids third-party cookie issues when Pentaract (cross-origin) calls this endpoint.
    // Store redirect URI and client_id for the callback to use
    if query.redirect_uri.is_some() || effective_client_id.is_some() {
        let redirect_uri = query.redirect_uri.as_deref().unwrap_or("");
        tracing::debug!(oauth_state = ?oauth_state, redirect_uri = %redirect_uri, client_id = ?effective_client_id, "storing redirect for state");

        if let Some(oauth_state) = oauth_state {
            let now = now_rfc3339();
            let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(10)).to_rfc3339();
            let sql = adapt_sql(
                "INSERT INTO happyview_auth_login_redirects (state, redirect_uri, client_id, created_at, expires_at) VALUES (?, ?, ?, ?, ?)",
                state.db_backend,
            );
            let _ = crate::db::query(&sql)
                .bind(&oauth_state)
                .bind(redirect_uri)
                .bind(effective_client_id.as_deref())
                .bind(&now)
                .bind(&expires_at)
                .execute(&state.db)
                .await;
        } else {
            tracing::warn!("no state key captured from OAuth authorize — redirect will be lost");
        }
    }

    Ok((jar, Json(serde_json::json!({ "url": url }))))
}

async fn callback(
    State(state): State<AppState>,
    jar: SignedCookieJar<Key>,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    // The callback sets the session cookie; refuse when its signing key is not
    // secure (mirrors the guard in `login`).
    if !state.config.session_secret_secure() {
        return Err(AppError::ServerMisconfigured(
            crate::auth::COOKIE_AUTH_DISABLED_MSG.into(),
        ));
    }

    tracing::debug!(state = ?query.state, "callback received");

    if let Some(ref oauth_state) = query.state {
        use crate::linked_repos::flow::PendingGrant;
        match crate::linked_repos::flow::take_pending_grant(&state, oauth_state).await? {
            PendingGrant::Grant { grant_id, origin } => {
                return linked_repo_callback(&state, &grant_id, origin, query).await;
            }
            PendingGrant::Expired => {
                tracing::warn!(
                    state = %oauth_state,
                    "linked-repo callback arrived for an expired or already-claimed authorization"
                );
                return Ok(crate::linked_repos::flow::link_result_redirect(
                    &state, "expired", None,
                ));
            }
            PendingGrant::NotLinked => {}
        }
    }

    // Look up the redirect URI and client_id from the database before the OAuth library consumes the state
    let (redirect_url, client_id) = if let Some(oauth_state) = &query.state {
        let sql = adapt_sql(
            "SELECT redirect_uri, client_id FROM happyview_auth_login_redirects WHERE state = ? AND expires_at > ?",
            state.db_backend,
        );
        let now = now_rfc3339();
        let row: Option<(String, Option<String>)> = crate::db::query_as(&sql)
            .bind(oauth_state)
            .bind(&now)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

        // Clean up the row (one-time use)
        if row.is_some() {
            let delete_sql = adapt_sql(
                "DELETE FROM happyview_auth_login_redirects WHERE state = ?",
                state.db_backend,
            );
            let _ = crate::db::query(&delete_sql)
                .bind(oauth_state)
                .execute(&state.db)
                .await;
        }

        tracing::debug!(found_redirect = ?row, "redirect lookup result");
        match row {
            Some((uri, cid)) => {
                let uri = if uri.is_empty() { None } else { Some(uri) };
                (uri, cid)
            }
            None => (None, None),
        }
    } else {
        tracing::debug!("no state in callback query");
        (None, None)
    };

    // Use the same OAuth client that was used for authorize
    let oauth_client = state.oauth.get_or_default(client_id.as_deref());

    let params = atrium_oauth::CallbackParams {
        code: query.code,
        state: query.state,
        iss: query.iss,
    };

    let (session, _app_state) = oauth_client
        .callback(params)
        .await
        .map_err(|e| AppError::Internal(format!("OAuth callback failed: {e}")))?;

    use atrium_api::agent::SessionManager;
    let did = session
        .did()
        .await
        .ok_or_else(|| AppError::Internal("no DID in OAuth session".into()))?;

    // Check if the user is authorized to access the dashboard.
    // Allow login when no users exist yet (first user will be bootstrapped as admin).
    // Also allow login for the configured attached account DID (setup attach-auth flow).
    // Otherwise, only allow users already in the users table.
    let user_count: (i64,) = crate::db::query_as("SELECT COUNT(*) FROM happyview_users")
        .fetch_one(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("user count query failed: {e}")))?;

    if user_count.0 > 0 {
        let user_exists: Option<(i32,)> = crate::db::query_as(&adapt_sql(
            "SELECT 1 FROM happyview_users WHERE did = ?",
            state.db_backend,
        ))
        .bind(did.as_ref())
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("user lookup failed: {e}")))?;

        if user_exists.is_none() {
            // Allow login if this DID is the configured attached account (setup flow)
            let is_attached_account: Option<(i32,)> = crate::db::query_as(&adapt_sql(
                "SELECT 1 FROM happyview_service_identity WHERE attached_account_did = ?",
                state.db_backend,
            ))
            .bind(did.as_ref())
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

            if is_attached_account.is_none() {
                let login_url = state
                    .config
                    .base_path
                    .as_ref()
                    .map(|bp| format!("{}/login?error=not_authorized", bp))
                    .unwrap_or_else(|| "/login?error=not_authorized".into());
                return Ok((jar, Redirect::to(&login_url)).into_response());
            }
        }
    }

    // Look up the client_key for the API client so we can store it in the session cookie
    // for per-client rate limiting.
    let client_key = if let Some(ref cid) = client_id {
        let sql = adapt_sql(
            "SELECT client_key FROM happyview_api_clients WHERE client_id_url = ? AND is_active = 1",
            state.db_backend,
        );
        let row: Option<(String,)> = crate::db::query_as(&sql)
            .bind(cid)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);
        row.map(|(k,)| k)
    } else {
        None
    };

    let default_redirect = state
        .config
        .base_path
        .as_ref()
        .map(|bp| format!("{}/dashboard/", bp))
        .unwrap_or_else(|| "/dashboard/".into());
    let redirect_url = redirect_url.unwrap_or(default_redirect);
    tracing::debug!(redirect_url = %redirect_url, "redirecting after callback");

    // Set the session cookie
    // Must use SameSite=None for cross-origin requests (e.g., Pentaract calling HappyView)
    // Encode did and optional client_key separated by newline.
    let did_str = did.as_ref();
    let cookie_value = if let Some(ref ck) = client_key {
        format!("{did_str}\n{ck}")
    } else {
        did_str.to_string()
    };
    let secure = is_https(&state.config.public_url);
    let same_site = if secure {
        axum_extra::extract::cookie::SameSite::None
    } else {
        axum_extra::extract::cookie::SameSite::Lax
    };
    let mut session_cookie = Cookie::new(COOKIE_NAME, cookie_value);
    session_cookie.set_path("/");
    session_cookie.set_http_only(true);
    session_cookie.set_same_site(same_site);
    session_cookie.set_secure(secure);

    // Remove the legacy redirect cookie if present (old cookie-based approach)
    let jar = if jar.get(LEGACY_REDIRECT_COOKIE).is_some() {
        let mut removal = Cookie::from(LEGACY_REDIRECT_COOKIE);
        removal.set_path("/");
        removal.set_same_site(same_site);
        removal.set_secure(secure);
        jar.add(session_cookie).remove(removal)
    } else {
        jar.add(session_cookie)
    };

    Ok((jar, Redirect::to(&redirect_url)).into_response())
}

async fn linked_repo_callback(
    state: &AppState,
    grant_id: &str,
    origin: crate::linked_repos::flow::AuthOrigin,
    query: CallbackQuery,
) -> Result<Response, AppError> {
    use crate::linked_repos::flow;
    use crate::linked_repos::flow::AuthOrigin;

    let finish = |status: &str, handle: Option<&str>| -> Response {
        match origin {
            AuthOrigin::Admin if status == "success" => {
                let dashboard = format!(
                    "{}/dashboard/settings/linked-repos",
                    state.config.effective_public_url().trim_end_matches('/')
                );
                Redirect::to(&dashboard).into_response()
            }
            _ => flow::link_result_redirect(state, status, handle),
        }
    };

    let grant = match crate::linked_repos::db::get(state, grant_id).await? {
        Some(grant) => grant,
        None => return Ok(finish("gone", None)),
    };

    let client = flow::client_for_grant(state, &grant)?;

    let params = atrium_oauth::CallbackParams {
        code: query.code,
        state: query.state,
        iss: query.iss,
    };

    let (session, _app_state) = match client.callback(params).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%grant_id, error = %e, "linked repo OAuth callback failed");
            return Ok(finish("failed", None));
        }
    };

    use atrium_api::agent::SessionManager;
    let did = session
        .did()
        .await
        .ok_or_else(|| AppError::Internal("linked repo session has no DID".into()))?
        .as_ref()
        .to_string();

    if let Err(e) = flow::complete(state, grant_id, &did).await {
        if let Err(cleanup) = flow::discard_session(state, &did).await {
            tracing::error!(%did, error = %cleanup, "failed to discard refused linked-repo session");
        }
        let status = match &e {
            AppError::Conflict(_) => "already_linked",
            _ => "mismatch",
        };
        tracing::warn!(%grant_id, %did, error = %e, "linked repo completion refused");
        return Ok(finish(status, None));
    }

    match flow::invalidate_invites_for_grant(state, grant_id).await {
        Ok(n) if n > 0 => {
            tracing::debug!(%grant_id, count = n, "cleared invites for a now-linked grant")
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!(%grant_id, error = %e, "failed to clear invites after linking")
        }
    }

    Ok(finish(
        "success",
        grant.handle.as_deref().or(Some(did.as_str())),
    ))
}

async fn logout(
    State(state): State<AppState>,
    jar: SignedCookieJar<Key>,
) -> Result<SignedCookieJar<Key>, AppError> {
    if let Some(cookie) = jar.get(COOKIE_NAME) {
        let raw = cookie.value().to_string();
        let did_str = raw.split('\n').next().unwrap_or(&raw).to_string();
        if let Ok(did) = atrium_api::types::string::Did::new(did_str) {
            let _ = state.oauth.primary_client().revoke(&did).await;
        }
    }

    let secure = is_https(&state.config.public_url);
    let same_site = if secure {
        axum_extra::extract::cookie::SameSite::None
    } else {
        axum_extra::extract::cookie::SameSite::Lax
    };
    let mut removal = Cookie::from(COOKIE_NAME);
    removal.set_path("/");
    removal.set_same_site(same_site);
    removal.set_secure(secure);
    let jar = jar.remove(removal);
    Ok(jar)
}

#[derive(serde::Serialize)]
struct MeResponse {
    did: String,
    is_admin: bool,
}

async fn me(
    State(state): State<AppState>,
    jar: SignedCookieJar<Key>,
) -> Result<Json<MeResponse>, AppError> {
    let cookie = jar
        .get(COOKIE_NAME)
        .ok_or(AppError::Auth("not authenticated".into()))?;
    let raw = cookie.value().to_string();
    let did = raw.split('\n').next().unwrap_or(&raw).to_string();

    let backend = state.db_backend;
    let user: Option<(i32,)> = crate::db::query_as(&adapt_sql(
        "SELECT 1 FROM happyview_users WHERE did = ?",
        backend,
    ))
    .bind(&did)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("user lookup failed: {e}")))?;

    Ok(Json(MeResponse {
        did,
        is_admin: user.is_some(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scope_string_maps_known_scopes() {
        let scopes = parse_scope_string("atproto transition:generic transition:chat.bsky");
        assert_eq!(scopes.len(), 3);
        assert!(matches!(scopes[0], Scope::Known(KnownScope::Atproto)));
        assert!(matches!(
            scopes[1],
            Scope::Known(KnownScope::TransitionGeneric)
        ));
        assert!(matches!(
            scopes[2],
            Scope::Known(KnownScope::TransitionChatBsky)
        ));
    }

    #[test]
    fn parse_scope_string_treats_unknown_as_unknown() {
        let scopes = parse_scope_string("atproto include:games.gamesgamesgamesgames.authBasic");
        assert_eq!(scopes.len(), 2);
        assert!(matches!(scopes[0], Scope::Known(KnownScope::Atproto)));
        match &scopes[1] {
            Scope::Unknown(s) => {
                assert_eq!(s, "include:games.gamesgamesgamesgames.authBasic");
            }
            _ => panic!("expected Scope::Unknown for include: reference"),
        }
    }

    #[test]
    fn parse_scope_string_handles_extra_whitespace_and_empty_input() {
        let scopes = parse_scope_string("   atproto   \n\t  transition:generic  ");
        assert_eq!(scopes.len(), 2);
        assert!(matches!(scopes[0], Scope::Known(KnownScope::Atproto)));
        assert!(matches!(
            scopes[1],
            Scope::Known(KnownScope::TransitionGeneric)
        ));

        let empty = parse_scope_string("   \n  \t  ");
        assert!(empty.is_empty());
    }
}
