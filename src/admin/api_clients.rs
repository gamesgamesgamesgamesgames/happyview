use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use hex;
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};

use super::auth::UserAuth;
use super::permissions::Permission;
use super::types::{
    ApiClientSummary, CreateApiClientBody, CreateApiClientResponse, UpdateApiClientBody,
};

#[derive(Deserialize)]
pub(super) struct ListApiClientsQuery {
    pub(super) parent_id: Option<String>,
}

/// POST /admin/api-clients — create a new API client.
pub(super) async fn create_api_client(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<CreateApiClientBody>,
) -> Result<(StatusCode, Json<CreateApiClientResponse>), AppError> {
    auth.require(Permission::ApiClientsCreate).await?;

    if state.oauth.is_domain_client_id(&body.client_id_url) {
        return Err(AppError::Conflict(format!(
            "client_id_url '{}' conflicts with a registered domain's OAuth client",
            body.client_id_url
        )));
    }

    // Generate the client key: "hvc_" + 32 random hex chars.
    let mut random_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut random_bytes);
    let client_key = format!("hvc_{}", hex::encode(random_bytes));

    // Generate the client secret for confidential clients only.
    let (client_secret, client_secret_hash) = if body.client_type == "confidential" {
        let mut secret_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut secret_bytes);
        let secret = format!("hvs_{}", hex::encode(secret_bytes));
        let hash = hex::encode(Sha256::digest(secret.as_bytes()));
        (Some(secret), hash)
    } else {
        (None, String::new())
    };

    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let redirect_uris_json =
        serde_json::to_string(&body.redirect_uris).unwrap_or_else(|_| "[]".to_string());

    let allowed_origins_json = body
        .allowed_origins
        .as_ref()
        .map(|origins| serde_json::to_string(origins).unwrap_or_else(|_| "[]".to_string()));

    let insert_sql = adapt_sql(
        "INSERT INTO happyview_api_clients (id, client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, scopes, rate_limit_capacity, rate_limit_refill_rate, client_type, allowed_origins, is_active, created_by, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
        state.db_backend,
    );

    crate::db::query(&insert_sql)
        .bind(&id)
        .bind(&client_key)
        .bind(&client_secret_hash)
        .bind(&body.name)
        .bind(&body.client_id_url)
        .bind(&body.client_uri)
        .bind(&redirect_uris_json)
        .bind(&body.scopes)
        .bind(body.rate_limit_capacity)
        .bind(body.rate_limit_refill_rate)
        .bind(&body.client_type)
        .bind(&allowed_origins_json)
        .bind(&auth.did)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create api client: {e}")))?;

    // Register the new client in the OAuth registry so it's usable immediately.
    let oauth_params = crate::auth::client_registry::ApiClientOAuthParams {
        plc_url: state.config.plc_url.clone(),
        state_store: state.oauth_state_store.clone(),
        session_store_pool: state.db.clone(),
        db_backend: state.db_backend,
        client_keys: None,
        signing_kid: None,
    };
    if let Err(e) = state.oauth.register_api_client(
        &body.client_id_url,
        &body.client_uri,
        body.redirect_uris.clone(),
        &body.scopes,
        &oauth_params,
    ) {
        tracing::warn!(client_id = %body.client_id_url, error = %e, "OAuth client registration failed (DB row created)");
    }

    // Register the client identity for request validation.
    state.rate_limiter.register_client_identity(
        client_key.clone(),
        crate::rate_limit::ClientIdentity {
            secret_hash: client_secret_hash.clone(),
            client_uri: body.client_uri.clone(),
        },
    );

    // Register per-client rate limit config if overrides are set.
    if let (Some(capacity), Some(refill_rate)) =
        (body.rate_limit_capacity, body.rate_limit_refill_rate)
    {
        let defaults = state.rate_limiter.defaults();
        state.rate_limiter.register_client_config(
            client_key.clone(),
            crate::rate_limit::RateLimitConfig {
                capacity: capacity as u32,
                refill_rate,
                default_query_cost: defaults.query_cost,
                default_procedure_cost: defaults.procedure_cost,
                default_proxy_cost: defaults.proxy_cost,
            },
        );
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "api_client.created".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(body.name.clone()),
            detail: serde_json::json!({
                "client_key": client_key,
                "client_id_url": body.client_id_url,
            }),
        },
        state.db_backend,
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(CreateApiClientResponse {
            id,
            client_key,
            client_secret,
            name: body.name,
            client_id_url: body.client_id_url,
            client_type: body.client_type,
        }),
    ))
}

/// GET /admin/api-clients — list all API clients.
pub(super) async fn list_api_clients(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(query): Query<ListApiClientsQuery>,
) -> Result<Json<Vec<ApiClientSummary>>, AppError> {
    auth.require(Permission::ApiClientsView).await?;

    let (select_sql, parent_filter) = if let Some(ref parent_id) = query.parent_id {
        (
            adapt_sql(
                "SELECT id, client_key, name, client_id_url, client_uri, redirect_uris, scopes, client_type, allowed_origins, rate_limit_capacity, rate_limit_refill_rate, is_active, created_by, created_at, updated_at, parent_client_id, owner_did FROM happyview_api_clients WHERE parent_client_id = ? ORDER BY created_at DESC",
                state.db_backend,
            ),
            Some(parent_id.clone()),
        )
    } else {
        (
            adapt_sql(
                "SELECT id, client_key, name, client_id_url, client_uri, redirect_uris, scopes, client_type, allowed_origins, rate_limit_capacity, rate_limit_refill_rate, is_active, created_by, created_at, updated_at, parent_client_id, owner_did FROM happyview_api_clients ORDER BY created_at DESC",
                state.db_backend,
            ),
            None,
        )
    };

    let q = crate::db::query(&select_sql);
    let q = if let Some(ref pid) = parent_filter {
        q.bind(pid)
    } else {
        q
    };

    let rows = q
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list api clients: {e}")))?;

    let clients: Vec<ApiClientSummary> = rows
        .into_iter()
        .map(|row| {
            let redirect_uris_json: String = row.get("redirect_uris");
            let allowed_origins_json: Option<String> = row.get("allowed_origins");
            let is_active: i32 = row.get("is_active");
            let redirect_uris: Vec<String> =
                serde_json::from_str(&redirect_uris_json).unwrap_or_default();
            let allowed_origins: Option<Vec<String>> = allowed_origins_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok());
            ApiClientSummary {
                id: row.get("id"),
                client_key: row.get("client_key"),
                name: row.get("name"),
                client_id_url: row.get("client_id_url"),
                client_uri: row.get("client_uri"),
                redirect_uris,
                scopes: row.get("scopes"),
                client_type: row.get("client_type"),
                allowed_origins,
                rate_limit_capacity: row.get("rate_limit_capacity"),
                rate_limit_refill_rate: row.get("rate_limit_refill_rate"),
                is_active: is_active != 0,
                created_by: row.get("created_by"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
                parent_client_id: row.get("parent_client_id"),
                owner_did: row.get("owner_did"),
            }
        })
        .collect();

    Ok(Json(clients))
}

/// GET /admin/api-clients/:id — get a single API client.
pub(super) async fn get_api_client(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<ApiClientSummary>, AppError> {
    auth.require(Permission::ApiClientsView).await?;

    let select_sql = adapt_sql(
        "SELECT id, client_key, name, client_id_url, client_uri, redirect_uris, scopes, client_type, allowed_origins, rate_limit_capacity, rate_limit_refill_rate, is_active, created_by, created_at, updated_at, parent_client_id, owner_did FROM happyview_api_clients WHERE id = ?",
        state.db_backend,
    );

    let row = crate::db::query(&select_sql)
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get api client: {e}")))?;

    let Some(row) = row else {
        return Err(AppError::NotFound(format!("api client '{id}' not found")));
    };

    let redirect_uris_json: String = row.get("redirect_uris");
    let allowed_origins_json: Option<String> = row.get("allowed_origins");
    let is_active: i32 = row.get("is_active");
    let redirect_uris: Vec<String> = serde_json::from_str(&redirect_uris_json).unwrap_or_default();
    let allowed_origins: Option<Vec<String>> = allowed_origins_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok());

    Ok(Json(ApiClientSummary {
        id: row.get("id"),
        client_key: row.get("client_key"),
        name: row.get("name"),
        client_id_url: row.get("client_id_url"),
        client_uri: row.get("client_uri"),
        redirect_uris,
        scopes: row.get("scopes"),
        client_type: row.get("client_type"),
        allowed_origins,
        rate_limit_capacity: row.get("rate_limit_capacity"),
        rate_limit_refill_rate: row.get("rate_limit_refill_rate"),
        is_active: is_active != 0,
        created_by: row.get("created_by"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        parent_client_id: row.get("parent_client_id"),
        owner_did: row.get("owner_did"),
    }))
}

/// PUT /admin/api-clients/:id — update an API client.
pub(super) async fn update_api_client(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    Json(body): Json<UpdateApiClientBody>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::ApiClientsEdit).await?;

    // Read current values
    let select_sql = adapt_sql(
        "SELECT client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, scopes, allowed_origins, rate_limit_capacity, rate_limit_refill_rate, is_active FROM happyview_api_clients WHERE id = ?",
        state.db_backend,
    );

    type UpdateRow = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<i32>,
        Option<f64>,
        i32,
    );
    let row: Option<UpdateRow> = crate::db::query_as(&select_sql)
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get api client: {e}")))?;

    let Some((
        client_key,
        client_secret_hash,
        cur_name,
        client_id_url,
        cur_client_uri,
        cur_redirect_uris,
        cur_scopes,
        cur_allowed_origins,
        cur_capacity,
        cur_refill,
        cur_active,
    )) = row
    else {
        return Err(AppError::NotFound(format!("api client '{id}' not found")));
    };

    let name = body.name.unwrap_or(cur_name);
    let client_uri = body.client_uri.unwrap_or(cur_client_uri);
    let redirect_uris_json = body
        .redirect_uris
        .map(|uris| serde_json::to_string(&uris).unwrap_or_else(|_| "[]".to_string()))
        .unwrap_or(cur_redirect_uris);
    let scopes = body.scopes.unwrap_or(cur_scopes);
    let allowed_origins_json: Option<String> = match body.allowed_origins {
        Some(Some(origins)) => {
            Some(serde_json::to_string(&origins).unwrap_or_else(|_| "[]".to_string()))
        }
        Some(None) => None,
        None => cur_allowed_origins,
    };
    let capacity = body.rate_limit_capacity.unwrap_or(cur_capacity);
    let refill_rate = body.rate_limit_refill_rate.unwrap_or(cur_refill);
    let is_active = body
        .is_active
        .map(|a| if a { 1i32 } else { 0i32 })
        .unwrap_or(cur_active);
    let now = now_rfc3339();

    let update_sql = adapt_sql(
        "UPDATE happyview_api_clients SET name = ?, client_uri = ?, redirect_uris = ?, scopes = ?, allowed_origins = ?, rate_limit_capacity = ?, rate_limit_refill_rate = ?, is_active = ?, updated_at = ? WHERE id = ?",
        state.db_backend,
    );

    crate::db::query(&update_sql)
        .bind(&name)
        .bind(&client_uri)
        .bind(&redirect_uris_json)
        .bind(&scopes)
        .bind(&allowed_origins_json)
        .bind(capacity)
        .bind(refill_rate)
        .bind(is_active)
        .bind(&now)
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update api client: {e}")))?;

    // Re-register or remove from OAuth registry based on active status.
    let oauth_params = crate::auth::client_registry::ApiClientOAuthParams {
        plc_url: state.config.plc_url.clone(),
        state_store: state.oauth_state_store.clone(),
        session_store_pool: state.db.clone(),
        db_backend: state.db_backend,
        // Third-party client: it publishes its own metadata document, so we
        // must not declare private_key_jwt on its behalf.
        client_keys: None,
        signing_kid: None,
    };
    if is_active != 0 {
        let redirect_uris: Vec<String> =
            serde_json::from_str(&redirect_uris_json).unwrap_or_default();
        if let Err(e) = state.oauth.register_api_client(
            &client_id_url,
            &client_uri,
            redirect_uris,
            &scopes,
            &oauth_params,
        ) {
            tracing::warn!(client_id = %client_id_url, error = %e, "OAuth client re-registration failed");
        }
    } else {
        state.oauth.remove(&client_id_url);
    }

    // Update client identity and per-client rate limit config.
    if is_active != 0 {
        state.rate_limiter.register_client_identity(
            client_key.clone(),
            crate::rate_limit::ClientIdentity {
                secret_hash: client_secret_hash,
                client_uri: client_uri.clone(),
            },
        );
        if let (Some(cap), Some(refill)) = (capacity, refill_rate) {
            let defaults = state.rate_limiter.defaults();
            state.rate_limiter.register_client_config(
                client_key,
                crate::rate_limit::RateLimitConfig {
                    capacity: cap as u32,
                    refill_rate: refill,
                    default_query_cost: defaults.query_cost,
                    default_procedure_cost: defaults.procedure_cost,
                    default_proxy_cost: defaults.proxy_cost,
                },
            );
        } else {
            // Rate limit overrides were cleared — remove per-client config.
            state.rate_limiter.remove_client_config(&client_key);
        }
    } else {
        state.rate_limiter.remove_client_identity(&client_key);
        state.rate_limiter.remove_client_config(&client_key);

        // Cascade deactivation to child clients.
        let deactivate_children_sql = adapt_sql(
            "UPDATE happyview_api_clients SET is_active = 0, updated_at = ? WHERE parent_client_id = ? AND is_active = 1",
            state.db_backend,
        );
        let _ = crate::db::query(&deactivate_children_sql)
            .bind(&now)
            .bind(&id)
            .execute(&state.db)
            .await;

        let children_sql = adapt_sql(
            "SELECT client_id_url, client_key FROM happyview_api_clients WHERE parent_client_id = ?",
            state.db_backend,
        );
        if let Ok(children) = crate::db::query_as::<(String, String)>(&children_sql)
            .bind(&id)
            .fetch_all(&state.db)
            .await
        {
            for (child_url, child_key) in children {
                state.oauth.remove(&child_url);
                state.rate_limiter.remove_client_config(&child_key);
                state.rate_limiter.remove_client_identity(&child_key);
            }
        }
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "api_client.updated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// Mint the client's ES256 authentication key, or return the existing one.
pub(super) async fn provision_auth_key(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::ApiClientsEdit).await?;

    let existing = crate::oauth::client_keys::load_keys(
        &state.db,
        state.db_backend,
        state.config.token_encryption_key.as_ref(),
        &id,
    )
    .await?;

    let kid = match existing
        .into_iter()
        .find(|k| k.status == crate::oauth::client_keys::KeyStatus::Current)
    {
        Some(key) => key.kid,
        None => {
            let key = crate::oauth::client_keys::generate_client_key(&id)?;
            crate::oauth::client_keys::insert_key(
                &state.db,
                state.db_backend,
                state.config.token_encryption_key.as_ref(),
                &key,
            )
            .await?;
            key.kid
        }
    };

    Ok(Json(serde_json::json!({
        "kid": kid,
        "jwks_uri": jwks_uri_for(&state, &id),
    })))
}

/// GET /admin/api-clients/:id/auth-key — the client's current auth key, if any.
pub(super) async fn get_auth_key(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::ApiClientsView).await?;

    let keys = crate::oauth::client_keys::load_keys(
        &state.db,
        state.db_backend,
        state.config.token_encryption_key.as_ref(),
        &id,
    )
    .await?;

    let key = keys
        .into_iter()
        .find(|k| k.status == crate::oauth::client_keys::KeyStatus::Current)
        .ok_or_else(|| AppError::NotFound("no client authentication key".into()))?;

    Ok(Json(serde_json::json!({
        "kid": key.kid,
        "jwks_uri": jwks_uri_for(&state, &id),
    })))
}

/// GET /admin/api-clients/:id/auth-keys — every key this client holds,
/// with status and live session count.
pub(super) async fn list_auth_keys(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::ApiClientsView).await?;

    let keys =
        crate::oauth::client_keys::list_keys_for_owner(&state.db, state.db_backend, &id).await?;
    let session_counts =
        crate::oauth::client_keys::session_counts_by_kid(&state.db, state.db_backend).await?;

    let keys: Vec<serde_json::Value> = keys
        .into_iter()
        .map(|k| {
            let session_count = session_counts.get(&k.kid).copied().unwrap_or(0);
            serde_json::json!({
                "kid": k.kid,
                "status": k.status.as_str(),
                "created_at": k.created_at,
                "session_count": session_count,
            })
        })
        .collect();

    Ok(Json(
        serde_json::json!({ "keys": keys, "jwks_uri": jwks_uri_for(&state, &id) }),
    ))
}

/// DELETE /admin/api-clients/:id/auth-key/:kid — revoke one of this client's
/// authentication keys.
pub(super) async fn revoke_auth_key(
    State(state): State<AppState>,
    auth: UserAuth,
    Path((id, kid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::ApiClientsEdit).await?;

    let keys =
        crate::oauth::client_keys::list_keys_for_owner(&state.db, state.db_backend, &id).await?;
    let Some(target) = keys.into_iter().find(|k| k.kid == kid) else {
        return Err(AppError::NotFound(format!(
            "authentication key '{kid}' not found for this client"
        )));
    };

    if target.status == crate::oauth::client_keys::KeyStatus::Current {
        return Err(AppError::BadRequest(
            "cannot revoke the current authentication key: it is the only key signing new \
             sessions for this client, and revoking it would leave the client unable to \
             authenticate at all. Rotate the key first (which demotes this one to 'retiring' \
             and mints a new 'current'), then revoke the retiring key."
                .to_string(),
        ));
    }

    let session_counts =
        crate::oauth::client_keys::session_counts_by_kid(&state.db, state.db_backend).await?;
    let sessions_destroyed = session_counts.get(&kid).copied().unwrap_or(0);

    let revoked =
        crate::oauth::client_keys::revoke_key(&state.db, state.db_backend, &id, &kid).await?;
    if !revoked {
        return Err(AppError::NotFound(format!(
            "authentication key '{kid}' not found for this client"
        )));
    }

    state.oauth.evict_kid(&kid);

    log_event(
        &state.db,
        EventLog {
            event_type: "api_client.auth_key_revoked".to_string(),
            severity: Severity::Warn,
            actor_did: Some(auth.did.clone()),
            subject: Some(id.clone()),
            detail: serde_json::json!({ "kid": kid, "sessions_destroyed": sessions_destroyed }),
        },
        state.db_backend,
    )
    .await;

    Ok(Json(
        serde_json::json!({ "kid": kid, "sessions_destroyed": sessions_destroyed }),
    ))
}

/// DELETE /admin/api-clients/:id/auth-keys — revoke EVERY key this client
/// holds, un-delegating it entirely.
pub(super) async fn revoke_all_auth_keys(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::ApiClientsEdit).await?;

    let keys =
        crate::oauth::client_keys::list_keys_for_owner(&state.db, state.db_backend, &id).await?;
    let live: Vec<_> = keys
        .into_iter()
        .filter(|k| k.status != crate::oauth::client_keys::KeyStatus::Revoked)
        .collect();

    if live.is_empty() {
        return Err(AppError::BadRequest(
            "this client holds no authentication key; there is nothing to revoke".into(),
        ));
    }

    let session_counts =
        crate::oauth::client_keys::session_counts_by_kid(&state.db, state.db_backend).await?;
    let sessions_destroyed: u64 = live
        .iter()
        .map(|k| session_counts.get(&k.kid).copied().unwrap_or(0))
        .sum();

    crate::oauth::client_keys::revoke_keys_for_owner(&state.db, state.db_backend, &id).await?;

    // Every kid, not just the current one — any of them could still be held
    // by an in-memory client that would otherwise keep signing until restart.
    for key in &live {
        state.oauth.evict_kid(&key.kid);
    }

    let revoked: Vec<&str> = live.iter().map(|k| k.kid.as_str()).collect();
    log_event(
        &state.db,
        EventLog {
            event_type: "api_client.auth_keys_revoked_all".to_string(),
            severity: Severity::Warn,
            actor_did: Some(auth.did.clone()),
            subject: Some(id.clone()),
            detail: serde_json::json!({
                "kids": revoked,
                "sessions_destroyed": sessions_destroyed,
            }),
        },
        state.db_backend,
    )
    .await;

    Ok(Json(serde_json::json!({
        "revoked": live.len(),
        "sessions_destroyed": sessions_destroyed,
    })))
}

/// POST /admin/api-clients/:id/auth-key/rotate — rotate the client's
/// authentication key, demoting the current one to `retiring` rather than
/// revoking it, so already-established sessions keep resolving until they
/// naturally cycle off it (see `client_keys::rotate_key`'s doc comment).
pub(super) async fn rotate_auth_key(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::ApiClientsEdit).await?;

    let key = crate::oauth::client_keys::rotate_key(
        &state.db,
        state.db_backend,
        state.config.token_encryption_key.as_ref(),
        &id,
    )
    .await?;

    let orphaned_sessions =
        crate::oauth::client_keys::count_unstamped_sessions(&state.db, state.db_backend, &id)
            .await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "api_client.auth_key_rotated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id),
            detail: serde_json::json!({
                "kid": key.kid,
                "orphaned_sessions": orphaned_sessions,
            }),
        },
        state.db_backend,
    )
    .await;

    Ok(Json(serde_json::json!({
        "kid": key.kid,
        "orphaned_sessions": orphaned_sessions,
    })))
}

/// POST /admin/oauth/instance-key/rotate — rotate the instance's own OAuth
/// client-authentication key and immediately rebuild the OAuth clients
/// pinned to it (primary + every domain), so the new key takes effect
/// without a restart. See `oauth::rotation::rotate_instance_key`.
pub(super) async fn rotate_instance_key(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let (key, orphaned_sessions) = crate::oauth::rotation::rotate_instance_key(&state).await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "oauth.instance_key_rotated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: None,
            detail: serde_json::json!({
                "kid": key.kid,
                "orphaned_sessions": orphaned_sessions,
            }),
        },
        state.db_backend,
    )
    .await;

    Ok(Json(serde_json::json!({
        "kid": key.kid,
        "orphaned_sessions": orphaned_sessions,
    })))
}

/// GET /admin/oauth/instance-key — list the instance's client-authentication
/// keys, including revoked ones. An operator responding to a leak needs to
/// see that a key IS revoked; a listing that hides revoked keys makes the
/// revoke button's effect invisible.
pub(super) async fn list_instance_keys(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let keys = crate::oauth::client_keys::list_keys_for_owner(
        &state.db,
        state.db_backend,
        crate::oauth::client_keys::INSTANCE_OWNER,
    )
    .await?;
    let session_counts =
        crate::oauth::client_keys::session_counts_by_kid(&state.db, state.db_backend).await?;

    let keys: Vec<serde_json::Value> = keys
        .into_iter()
        .map(|k| {
            let session_count = session_counts.get(&k.kid).copied().unwrap_or(0);
            serde_json::json!({
                "kid": k.kid,
                "status": k.status.as_str(),
                "created_at": k.created_at,
                "session_count": session_count,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "keys": keys })))
}

/// DELETE /admin/oauth/instance-key/:kid — revoke a single instance key.
pub(super) async fn revoke_instance_key(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(kid): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let owner = crate::oauth::client_keys::INSTANCE_OWNER;
    let keys =
        crate::oauth::client_keys::list_keys_for_owner(&state.db, state.db_backend, owner).await?;
    let Some(target) = keys.into_iter().find(|k| k.kid == kid) else {
        return Err(AppError::NotFound(format!(
            "instance key '{kid}' not found"
        )));
    };

    if target.status == crate::oauth::client_keys::KeyStatus::Current {
        return Err(AppError::BadRequest(
            "cannot revoke the current instance key: it is the only key signing new sessions, \
             and revoking it now would leave the instance unable to authenticate to any PDS. \
             Rotate the key first (which demotes this one to 'retiring' and mints a new \
             'current'), then revoke the retiring key."
                .to_string(),
        ));
    }

    let session_counts =
        crate::oauth::client_keys::session_counts_by_kid(&state.db, state.db_backend).await?;
    let sessions_destroyed = session_counts.get(&kid).copied().unwrap_or(0);

    let revoked =
        crate::oauth::client_keys::revoke_key(&state.db, state.db_backend, owner, &kid).await?;
    if !revoked {
        // Lost a race with a concurrent revoke between the lookup above and
        // this update — the key is gone either way.
        return Err(AppError::NotFound(format!(
            "instance key '{kid}' not found"
        )));
    }

    state.oauth.evict_kid(&kid);

    log_event(
        &state.db,
        EventLog {
            event_type: "oauth.instance_key_revoked".to_string(),
            severity: Severity::Warn,
            actor_did: Some(auth.did.clone()),
            subject: None,
            detail: serde_json::json!({
                "kid": kid,
                "sessions_destroyed": sessions_destroyed,
            }),
        },
        state.db_backend,
    )
    .await;

    Ok(Json(serde_json::json!({
        "kid": kid,
        "sessions_destroyed": sessions_destroyed,
    })))
}

fn registration_constraint_reason(client_id_url: &str) -> Option<String> {
    if crate::auth::client_registry::is_loopback_url(client_id_url) {
        return Some(
            "this app's client_id_url is a loopback address (localhost/127.0.0.1). \
             Loopback clients always register as public OAuth clients — no published \
             document can make one confidential — so this is expected and there is \
             nothing to fix in the document."
                .to_string(),
        );
    }

    None
}

/// POST /admin/api-clients/:id/auth-key/recheck — re-probe the client's
/// published metadata document and re-register it if its confidentiality
/// verdict changed.
pub(super) async fn recheck_auth_key(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::ApiClientsEdit).await?;

    let client_id_url =
        crate::oauth::pds_write::lookup_client_id_url(&state.db, state.db_backend, &id).await?;

    let confidential = state
        .oauth
        .refresh_client_confidentiality(&state, &id, &client_id_url)
        .await?;

    let probe = crate::oauth::client_probe::cached(&state, &id, &client_id_url).await?;
    let reason = if !confidential && probe.confidential {
        registration_constraint_reason(&client_id_url).unwrap_or(probe.reason)
    } else {
        probe.reason
    };

    Ok(Json(serde_json::json!({
        "confidential": confidential,
        "reason": reason,
        "checked_at": probe.checked_at,
    })))
}

pub(crate) fn jwks_uri_for(state: &AppState, api_client_id: &str) -> String {
    format!(
        "{}/oauth/clients/{}/jwks.json",
        state.config.effective_public_url().trim_end_matches('/'),
        api_client_id
    )
}

/// DELETE /admin/api-clients/:id — delete an API client.
pub(super) async fn delete_api_client(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::ApiClientsDelete).await?;

    // Revoke any client-authentication key before the row is deleted, so a
    // deleted client's key cannot keep signing or appearing in its JWKS.
    crate::oauth::client_keys::revoke_keys_for_owner(&state.db, state.db_backend, &id).await?;

    // Look up client_id_url and client_key before deleting so we can remove from registries.
    let lookup_sql = adapt_sql(
        "SELECT client_id_url, client_key FROM happyview_api_clients WHERE id = ?",
        state.db_backend,
    );
    let client_info: Option<(String, String)> = crate::db::query_as(&lookup_sql)
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up api client: {e}")))?;

    // Look up child clients before deleting (ON DELETE CASCADE will remove DB rows).
    let children_sql = adapt_sql(
        "SELECT client_id_url, client_key FROM happyview_api_clients WHERE parent_client_id = ?",
        state.db_backend,
    );
    let children: Vec<(String, String)> = crate::db::query_as(&children_sql)
        .bind(&id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

    let delete_sql = adapt_sql(
        "DELETE FROM happyview_api_clients WHERE id = ?",
        state.db_backend,
    );

    let result = crate::db::query(&delete_sql)
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete api client: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("api client '{id}' not found")));
    }

    // Remove parent from OAuth registry, rate limiter, and client identities.
    if let Some((url, key)) = client_info {
        state.oauth.remove(&url);
        state.rate_limiter.remove_client_config(&key);
        state.rate_limiter.remove_client_identity(&key);
    }

    // Remove child clients from in-memory registries (DB rows already cascaded).
    for (child_url, child_key) in &children {
        state.oauth.remove(child_url);
        state.rate_limiter.remove_client_config(child_key);
        state.rate_limiter.remove_client_identity(child_key);
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "api_client.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_client_key_prefix() {
        let mut random_bytes = [0u8; 16];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut random_bytes);
        let key = format!("hvc_{}", hex::encode(random_bytes));
        assert!(key.starts_with("hvc_"));
        assert_eq!(key.len(), 4 + 32); // "hvc_" + 32 hex chars
    }
}
