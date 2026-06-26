use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rand::Rng;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::CreateApiClientInput;
use crate::AppState;
use crate::auth::XrpcClaims;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::rate_limit::CheckResult;

pub async fn create_api_client(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<CreateApiClientInput>,
) -> Result<Response, AppError> {
    // 1. Require DPoP auth
    let claims = xrpc_claims
        .identity
        .ok_or_else(|| AppError::Auth("createApiClient requires DPoP authentication".into()))?;

    // 2. Rate-limit the request (procedure type)
    let check = if let Some(client_key) = claims.client_key() {
        let cost = state
            .rate_limiter
            .default_cost_for_type(client_key, "procedure");
        Some(state.rate_limiter.check(client_key, cost))
    } else {
        None
    };

    if let Some(CheckResult::Limited {
        retry_after,
        limit,
        reset,
    }) = check
    {
        return Err(AppError::RateLimited {
            retry_after,
            limit,
            reset,
        });
    }

    // 3. Get client_key from claims, resolve parent client, verify top-level
    let client_key_str = claims
        .client_key()
        .ok_or_else(|| AppError::Auth("createApiClient requires an API client key".into()))?;

    let parent_client = crate::oauth::client_auth::resolve_client_by_key(
        &state.db,
        state.db_backend,
        client_key_str,
    )
    .await
    .map_err(|_| AppError::Auth("Invalid client".into()))?;

    let parent_check_sql = adapt_sql(
        "SELECT parent_client_id, created_by FROM happyview_api_clients WHERE id = ?",
        state.db_backend,
    );
    let parent_row: Option<(Option<String>, String)> = sqlx::query_as(&parent_check_sql)
        .bind(&parent_client.id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to check parent status: {e}")))?;

    match parent_row {
        Some((Some(_), _)) => {
            return Err(AppError::Forbidden(
                "Child clients cannot create API clients".into(),
            ));
        }
        Some((None, _)) => { /* ok, top-level */ }
        None => return Err(AppError::Auth("Invalid client".into())),
    };

    let user_did = claims.did().to_string();

    // 4. Validate client_type
    if input.client_type != "confidential" && input.client_type != "public" {
        return Err(AppError::BadRequest(
            "client_type must be 'confidential' or 'public'".into(),
        ));
    }

    // 5. Check for duplicate client_id_url
    let dup_check_sql = adapt_sql(
        "SELECT id FROM happyview_api_clients WHERE client_id_url = ?",
        state.db_backend,
    );
    let dup: Option<(String,)> = sqlx::query_as(&dup_check_sql)
        .bind(&input.client_id_url)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to check client_id_url: {e}")))?;

    if dup.is_some() {
        return Err(AppError::Conflict(
            "client_id_url already registered".into(),
        ));
    }

    // 6. Generate `hvc_` client key and optional `hvs_` client secret
    let mut random_bytes = [0u8; 16];
    rand::rng().fill(&mut random_bytes);
    let child_client_key = format!("hvc_{}", hex::encode(random_bytes));

    let (client_secret, client_secret_hash) = if input.client_type == "confidential" {
        let mut secret_bytes = [0u8; 32];
        rand::rng().fill(&mut secret_bytes);
        let secret = format!("hvs_{}", hex::encode(secret_bytes));
        let hash = hex::encode(Sha256::digest(secret.as_bytes()));
        (Some(secret), hash)
    } else {
        (None, String::new())
    };

    // 7. Insert into `api_clients` table
    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let redirect_uris_json =
        serde_json::to_string(&input.redirect_uris).unwrap_or_else(|_| "[]".to_string());
    let allowed_origins_json = input
        .allowed_origins
        .as_ref()
        .map(|origins| serde_json::to_string(origins).unwrap_or_else(|_| "[]".to_string()));

    let insert_sql = adapt_sql(
        "INSERT INTO happyview_api_clients (id, client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, scopes, rate_limit_capacity, rate_limit_refill_rate, client_type, allowed_origins, is_active, created_by, created_at, updated_at, parent_client_id, owner_did) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?, ?, 1, ?, ?, ?, ?, ?)",
        state.db_backend,
    );
    sqlx::query(&insert_sql)
        .bind(&id)
        .bind(&child_client_key)
        .bind(&client_secret_hash)
        .bind(&input.name)
        .bind(&input.client_id_url)
        .bind(&input.client_uri)
        .bind(&redirect_uris_json)
        .bind(&input.scopes)
        .bind(&input.client_type)
        .bind(&allowed_origins_json)
        .bind(&user_did) // created_by
        .bind(&now) // created_at
        .bind(&now) // updated_at
        .bind(&parent_client.id) // parent_client_id
        .bind(&user_did) // owner_did
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create api client: {e}")))?;

    // 8. Register with OAuth registry and rate limiter
    let oauth_params = crate::auth::client_registry::ApiClientOAuthParams {
        plc_url: state.config.plc_url.clone(),
        state_store: state.oauth_state_store.clone(),
        session_store_pool: state.db.clone(),
        db_backend: state.db_backend,
    };
    if let Err(e) = state.oauth.register_api_client(
        &input.client_id_url,
        &input.client_uri,
        input.redirect_uris.clone(),
        &input.scopes,
        &oauth_params,
    ) {
        tracing::warn!(
            client_id = %input.client_id_url,
            error = %e,
            "OAuth client registration failed (DB row created)"
        );
    }

    state.rate_limiter.register_client_identity(
        child_client_key.clone(),
        crate::rate_limit::ClientIdentity {
            secret_hash: client_secret_hash.clone(),
            client_uri: input.client_uri.clone(),
        },
    );

    let defaults = state.rate_limiter.defaults();
    state.rate_limiter.register_client_config(
        child_client_key.clone(),
        crate::rate_limit::RateLimitConfig {
            capacity: state.config.default_rate_limit_capacity,
            refill_rate: state.config.default_rate_limit_refill_rate,
            default_query_cost: defaults.query_cost,
            default_procedure_cost: defaults.procedure_cost,
            default_proxy_cost: defaults.proxy_cost,
        },
    );

    // 9. Log event
    log_event(
        &state.db,
        EventLog {
            event_type: "api_client.created".to_string(),
            severity: Severity::Info,
            actor_did: Some(user_did.clone()),
            subject: Some(input.name.clone()),
            detail: serde_json::json!({
                "client_key": child_client_key,
                "client_id_url": input.client_id_url,
                "parent_client_id": parent_client.id,
                "self_service": true,
            }),
        },
        state.db_backend,
    )
    .await;

    // 10. Return { client: ApiClientView, clientSecret?: string }
    let view = super::ApiClientView {
        id,
        name: input.name,
        client_key: child_client_key,
        client_id_url: input.client_id_url,
        client_uri: input.client_uri,
        redirect_uris: input.redirect_uris,
        client_type: input.client_type,
        scopes: input.scopes,
        allowed_origins: input.allowed_origins.unwrap_or_default(),
        is_active: true,
        created_at: now,
    };

    let mut body = serde_json::json!({ "client": view });
    if let Some(ref secret) = client_secret {
        body["clientSecret"] = serde_json::json!(secret);
    }

    let mut response = Json(body).into_response();
    *response.status_mut() = StatusCode::CREATED;

    if let Some(CheckResult::Allowed {
        remaining,
        limit,
        reset,
    }) = check
    {
        let h = response.headers_mut();
        h.insert("RateLimit-Limit", limit.into());
        h.insert("RateLimit-Remaining", remaining.into());
        h.insert("RateLimit-Reset", reset.into());
    }

    Ok(response)
}
