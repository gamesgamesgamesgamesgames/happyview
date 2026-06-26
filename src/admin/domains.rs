use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::domain::Domain;
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};

use super::auth::UserAuth;
use super::permissions::Permission;
use super::types::{CreateDomainBody, DomainResponse};

fn domain_to_response(d: &Domain) -> DomainResponse {
    DomainResponse {
        id: d.id.clone(),
        url: d.url.clone(),
        is_primary: d.is_primary,
        created_at: d.created_at.clone(),
        updated_at: d.updated_at.clone(),
    }
}

/// GET /admin/domains
pub(super) async fn list(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<DomainResponse>>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let sql = adapt_sql(
        "SELECT id, url, is_primary, created_at, updated_at FROM happyview_domains ORDER BY created_at",
        state.db_backend,
    );
    let rows: Vec<(String, String, i32, String, String)> = sqlx::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list domains: {e}")))?;

    let domains: Vec<DomainResponse> = rows
        .into_iter()
        .map(
            |(id, url, is_primary, created_at, updated_at)| DomainResponse {
                id,
                url,
                is_primary: is_primary != 0,
                created_at,
                updated_at,
            },
        )
        .collect();

    Ok(Json(domains))
}

/// POST /admin/domains
pub(super) async fn create(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<CreateDomainBody>,
) -> Result<(StatusCode, Json<DomainResponse>), AppError> {
    auth.require(Permission::SettingsManage).await?;

    let url = body.url.trim_end_matches('/').to_string();

    let parsed =
        reqwest::Url::parse(&url).map_err(|_| AppError::BadRequest("invalid URL".into()))?;

    if parsed.path() != "/" && !parsed.path().is_empty() {
        return Err(AppError::BadRequest("URL must not contain a path".into()));
    }

    if parsed.host_str().is_none() {
        return Err(AppError::BadRequest("URL must contain a host".into()));
    }

    let is_loopback = state.config.public_url.contains("127.0.0.1")
        || state.config.public_url.contains("[::1]")
        || state.config.public_url.contains("localhost");

    if parsed.scheme() != "https" && !is_loopback {
        return Err(AppError::BadRequest("URL scheme must be https".into()));
    }

    // Check for duplicates
    let existing: Option<(String,)> = sqlx::query_as(&adapt_sql(
        "SELECT id FROM happyview_domains WHERE url = ?",
        state.db_backend,
    ))
    .bind(&url)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to check domain: {e}")))?;

    if existing.is_some() {
        return Err(AppError::BadRequest(format!(
            "domain '{url}' already exists"
        )));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_rfc3339();

    let sql = adapt_sql(
        "INSERT INTO happyview_domains (id, url, is_primary, created_at, updated_at) VALUES (?, ?, 0, ?, ?)",
        state.db_backend,
    );
    sqlx::query(&sql)
        .bind(&id)
        .bind(&url)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create domain: {e}")))?;

    let domain = Domain {
        id: id.clone(),
        url: url.clone(),
        is_primary: false,
        created_at: now.clone(),
        updated_at: now,
    };

    // Build the base-path-aware URLs for this domain
    let domain_base_url = state.config.url_with_base_path(&url);
    let client_id_url = format!(
        "{}/oauth-client-metadata.json",
        domain_base_url.trim_end_matches('/')
    );

    // Register the OAuth client for this domain
    state.oauth.register_domain_client(
        url.clone(),
        client_id_url.clone(),
        state.oauth.primary_client(),
    );

    // Build a proper OAuth client if not loopback
    let domain_is_loopback =
        url.contains("127.0.0.1") || url.contains("[::1]") || url.contains("localhost");
    if !domain_is_loopback {
        let callback = format!("{}/auth/callback", domain_base_url.trim_end_matches('/'));
        if let Err(e) = state.oauth.register_api_client(
            &client_id_url,
            &url,
            vec![callback],
            "atproto",
            &crate::auth::client_registry::ApiClientOAuthParams {
                plc_url: state.config.plc_url.clone(),
                state_store: state.oauth_state_store.clone(),
                session_store_pool: state.db.clone(),
                db_backend: state.db_backend,
            },
        ) {
            tracing::error!(domain = %url, error = %e, "Failed to create OAuth client for domain");
        } else {
            // Move from `clients` (where register_api_client puts it) to domain_clients + clients
            if let Some(client) = state.oauth.get(&client_id_url) {
                state.oauth.remove(&client_id_url);
                state
                    .oauth
                    .register_domain_client(url.clone(), client_id_url, client);
            }
        }
    }

    // Update in-memory cache
    state.domain_cache.insert(domain.clone()).await;

    log_event(
        &state.db,
        EventLog {
            event_type: "domain.created".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(url),
            detail: serde_json::json!({ "id": id }),
        },
        state.db_backend,
    )
    .await;

    let response = domain_to_response(&domain);
    Ok((StatusCode::CREATED, Json(response)))
}

/// DELETE /admin/domains/{id}
pub(super) async fn delete(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let sql = adapt_sql(
        "SELECT id, url, is_primary, created_at, updated_at FROM happyview_domains WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String, String, i32, String, String)> = sqlx::query_as(&sql)
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to find domain: {e}")))?;

    let (_, url, is_primary, _, _) =
        row.ok_or_else(|| AppError::NotFound("domain not found".into()))?;

    if is_primary != 0 {
        return Err(AppError::BadRequest(
            "cannot delete the primary domain — set a different domain as primary first".into(),
        ));
    }

    let delete_sql = adapt_sql(
        "DELETE FROM happyview_domains WHERE id = ?",
        state.db_backend,
    );
    sqlx::query(&delete_sql)
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete domain: {e}")))?;

    // Remove OAuth client and cache entry
    let domain_base_url = state.config.url_with_base_path(&url);
    let client_id_url = format!(
        "{}/oauth-client-metadata.json",
        domain_base_url.trim_end_matches('/')
    );
    state.oauth.remove_domain_client(&url, &client_id_url);
    let host = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(&url);
    state.domain_cache.remove(host).await;

    log_event(
        &state.db,
        EventLog {
            event_type: "domain.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(url),
            detail: serde_json::json!({ "id": id }),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /admin/domains/{id}/primary
pub(super) async fn set_primary(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let sql = adapt_sql(
        "SELECT id, url, is_primary, created_at, updated_at FROM happyview_domains WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String, String, i32, String, String)> = sqlx::query_as(&sql)
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to find domain: {e}")))?;

    let (_, url, _, _, _) = row.ok_or_else(|| AppError::NotFound("domain not found".into()))?;

    let now = now_rfc3339();

    let unset_sql = adapt_sql(
        "UPDATE happyview_domains SET is_primary = 0, updated_at = ? WHERE is_primary = 1",
        state.db_backend,
    );
    sqlx::query(&unset_sql)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to unset primary: {e}")))?;

    let set_sql = adapt_sql(
        "UPDATE happyview_domains SET is_primary = 1, updated_at = ? WHERE id = ?",
        state.db_backend,
    );
    sqlx::query(&set_sql)
        .bind(&now)
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to set primary: {e}")))?;

    // Update cache
    let host = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(&url);
    state.domain_cache.set_primary(host).await;

    // Update OAuth primary client
    if let Some(client) = state.oauth.get_domain_client(&url) {
        state.oauth.set_primary_client(client);
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "domain.primary_changed".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(url),
            detail: serde_json::json!({ "id": id }),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}
