use axum::{
    Json, Router,
    extract::{Path, Query, State},
    response::Redirect,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::AppState;
use crate::auth::Claims;
use crate::error::AppError;
use crate::external_auth::{pds_write, state, tokens};
use crate::plugin::PluginExecutor;
use crate::plugin::sync::SyncProcessor;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/providers", get(list_providers))
        .route("/accounts", get(list_accounts))
        .route("/{plugin_id}/authorize", get(authorize))
        .route("/{plugin_id}/callback", get(callback))
        .route("/{plugin_id}/connect", post(connect_with_config))
        .route("/{plugin_id}/sync", post(sync))
        .route("/{plugin_id}/unlink", post(unlink))
}

#[derive(Serialize)]
struct ProviderInfo {
    id: String,
    name: String,
    icon_url: Option<String>,
    auth_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_schema: Option<serde_json::Value>,
}

async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProviderInfo>>, AppError> {
    let plugins = state.plugin_registry.list().await;

    let providers: Vec<ProviderInfo> = plugins
        .into_iter()
        .map(|p| ProviderInfo {
            id: p.info.id.clone(),
            name: p.info.name.clone(),
            icon_url: p.info.icon_url.clone(),
            auth_type: p.info.auth_type.clone(),
            config_schema: p.info.config_schema.clone(),
        })
        .collect();

    Ok(Json(providers))
}

async fn list_accounts(
    State(app_state): State<AppState>,
    claims: Claims,
) -> Result<Json<Vec<tokens::LinkedAccountSummary>>, AppError> {
    let accounts = tokens::list_linked_accounts(&app_state.db, app_state.db_backend, claims.did())
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(accounts))
}

#[derive(Deserialize)]
struct AuthorizeQuery {
    redirect_uri: String,
}

async fn authorize(
    State(app_state): State<AppState>,
    Path(plugin_id): Path<String>,
    Query(query): Query<AuthorizeQuery>,
    domain: Option<axum::extract::Extension<std::sync::Arc<crate::domain::Domain>>>,
    claims: Claims,
) -> Result<Json<serde_json::Value>, AppError> {
    let _plugin = app_state
        .plugin_registry
        .get(&plugin_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Plugin not found: {}", plugin_id)))?;

    // Generate state parameter for CSRF protection
    let state_param = uuid::Uuid::new_v4().to_string();

    // Store state -> user mapping for callback validation
    // Store the frontend's redirect_uri so we can redirect back after callback
    state::store_state(
        &app_state.db,
        app_state.db_backend,
        &state_param,
        claims.did(),
        &plugin_id,
        &query.redirect_uri,
    )
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    // Get plugin config (empty for now, could come from DB)
    let config = serde_json::Value::Null;

    // Load secrets from DB (with env var fallback)
    let secrets = load_plugin_secrets(
        &app_state.db,
        app_state.db_backend,
        app_state.config.token_encryption_key.as_ref(),
        &plugin_id,
    )
    .await;

    // Create executor and instance
    let executor = PluginExecutor::new(
        app_state.wasm_runtime.clone(),
        app_state.plugin_registry.clone(),
        app_state.db.clone(),
        app_state.db_backend,
        app_state.http.clone(),
        Arc::new(app_state.lexicons.clone()),
    );

    let mut instance = executor
        .instantiate(&plugin_id, &state_param, secrets, config.clone())
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // Build the backend callback URL for OpenID/OAuth return_to
    // This ensures the auth provider redirects back to the backend, not the frontend
    let domain_url = domain
        .map(|d| app_state.config.url_with_base_path(&d.0.url))
        .unwrap_or_else(|| app_state.config.effective_public_url());

    let callback_url = format!(
        "{}/external-auth/{}/callback",
        domain_url.trim_end_matches('/'),
        plugin_id
    );

    let authorize_url = instance
        .call_get_authorize_url(&state_param, &callback_url, &config)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "authorize_url": authorize_url,
        "state": state_param
    })))
}

fn redirect_with_params(base_uri: &str, params: &[(&str, &str)]) -> Redirect {
    let separator = if base_uri.contains('?') { "&" } else { "?" };
    let query: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    Redirect::to(&format!("{}{}{}", base_uri, separator, query))
}

async fn callback(
    State(app_state): State<AppState>,
    Path(plugin_id): Path<String>,
    Query(query_params): Query<HashMap<String, String>>,
) -> Result<Redirect, AppError> {
    // Phase 1: Extract state param — required for both OAuth and OpenID
    // For OAuth: "state" param
    // For OpenID 2.0: also "state" (we pass it via return_to query string)
    let state_param = query_params
        .get("state")
        .ok_or_else(|| AppError::BadRequest("Missing state".into()))?;

    // Check for error param (OAuth error response)
    if let Some(error) = query_params.get("error") {
        return Err(AppError::BadRequest(error.clone()));
    }

    // Phase 2: Consume state — after this we have a redirect_uri
    let stored_state = state::consume_state(&app_state.db, app_state.db_backend, state_param)
        .await
        .map_err(|_| AppError::BadRequest("Invalid or expired state".into()))?;

    // Phase 3: Verify plugin_id matches — redirect on mismatch
    if stored_state.plugin_id != plugin_id {
        return Ok(redirect_with_params(
            &stored_state.redirect_uri,
            &[("auth", "error"), ("message", "Connection failed")],
        ));
    }

    // Phase 4: Everything after this redirects on error (never returns HTTP error)
    match callback_inner(&app_state, &stored_state, &query_params).await {
        Ok(()) => Ok(redirect_with_params(
            &stored_state.redirect_uri,
            &[("auth", "success")],
        )),
        Err(e) => {
            tracing::error!(
                plugin_id = %plugin_id,
                did = %stored_state.did,
                "OAuth callback failed: {e:#}"
            );
            Ok(redirect_with_params(
                &stored_state.redirect_uri,
                &[("auth", "error"), ("message", "Connection failed")],
            ))
        }
    }
}

async fn callback_inner(
    app_state: &AppState,
    stored_state: &state::StoredState,
    params: &HashMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = serde_json::Value::Null;
    let secrets = load_plugin_secrets(
        &app_state.db,
        app_state.db_backend,
        app_state.config.token_encryption_key.as_ref(),
        &stored_state.plugin_id,
    )
    .await;

    let executor = PluginExecutor::new(
        app_state.wasm_runtime.clone(),
        app_state.plugin_registry.clone(),
        app_state.db.clone(),
        app_state.db_backend,
        app_state.http.clone(),
        Arc::new(app_state.lexicons.clone()),
    );

    let mut instance = executor
        .instantiate(
            &stored_state.plugin_id,
            &stored_state.did,
            secrets,
            config.clone(),
        )
        .await?;

    let token_set = instance.call_handle_callback(params, &config).await?;

    let profile = instance
        .call_get_profile(&token_set.access_token, &config)
        .await?;

    let expires_at = token_set.expires_at.map(|dt| dt.to_rfc3339());

    tokens::store_tokens(
        &app_state.db,
        app_state.db_backend,
        app_state.config.token_encryption_key.as_ref(),
        &stored_state.did,
        &stored_state.plugin_id,
        &profile.account_id,
        &token_set.access_token,
        token_set.refresh_token.as_deref(),
        Some(&token_set.token_type),
        None,
        expires_at.as_deref(),
    )
    .await?;

    Ok(())
}

/// Connect with user-provided config (for API key auth type)
#[derive(Deserialize)]
struct ConnectConfigBody {
    /// User-provided configuration matching the plugin's config_schema
    config: serde_json::Value,
}

async fn connect_with_config(
    State(app_state): State<AppState>,
    Path(plugin_id): Path<String>,
    claims: Claims,
    Json(body): Json<ConnectConfigBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user_did = claims.did();

    let plugin = app_state
        .plugin_registry
        .get(&plugin_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Plugin not found: {}", plugin_id)))?;

    // Verify this is an API key plugin
    if plugin.info.auth_type != "api_key" {
        return Err(AppError::BadRequest(
            "This endpoint is only for API key authentication".into(),
        ));
    }

    let secrets = load_plugin_secrets(
        &app_state.db,
        app_state.db_backend,
        app_state.config.token_encryption_key.as_ref(),
        &plugin_id,
    )
    .await;

    let executor = PluginExecutor::new(
        app_state.wasm_runtime.clone(),
        app_state.plugin_registry.clone(),
        app_state.db.clone(),
        app_state.db_backend,
        app_state.http.clone(),
        Arc::new(app_state.lexicons.clone()),
    );

    // For API key auth, we pass the user's config to handle_callback
    // The params are empty since there's no OAuth flow
    let mut instance = executor
        .instantiate(&plugin_id, user_did, secrets, body.config.clone())
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // Call handle_callback with empty params (for API key auth, the config contains the key)
    let empty_params = HashMap::new();
    let token_set = instance
        .call_handle_callback(&empty_params, &body.config)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // Get profile to get the account_id
    let profile = instance
        .call_get_profile(&token_set.access_token, &body.config)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // Format expires_at as RFC3339 string
    let expires_at = token_set.expires_at.map(|dt| dt.to_rfc3339());

    // Store encrypted tokens
    tokens::store_tokens(
        &app_state.db,
        app_state.db_backend,
        app_state.config.token_encryption_key.as_ref(),
        user_did,
        &plugin_id,
        &profile.account_id,
        &token_set.access_token,
        token_set.refresh_token.as_deref(),
        Some(&token_set.token_type),
        None,
        expires_at.as_deref(),
    )
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "status": "connected",
        "account_id": profile.account_id,
        "display_name": profile.display_name
    })))
}

async fn sync(
    State(app_state): State<AppState>,
    Path(plugin_id): Path<String>,
    claims: Claims,
) -> Result<Json<serde_json::Value>, AppError> {
    let user_did = claims.did();

    let config = serde_json::Value::Null;
    let secrets = load_plugin_secrets(
        &app_state.db,
        app_state.db_backend,
        app_state.config.token_encryption_key.as_ref(),
        &plugin_id,
    )
    .await;

    let executor = PluginExecutor::new(
        app_state.wasm_runtime.clone(),
        app_state.plugin_registry.clone(),
        app_state.db.clone(),
        app_state.db_backend,
        app_state.http.clone(),
        Arc::new(app_state.lexicons.clone()),
    );

    let mut instance = executor
        .instantiate(&plugin_id, user_did, secrets, config.clone())
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // Get decrypted access token from DB
    let stored = tokens::get_tokens(
        &app_state.db,
        app_state.db_backend,
        app_state.config.token_encryption_key.as_ref(),
        user_did,
        &plugin_id,
    )
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    let mut records = instance
        .call_sync_account(&stored.access_token, &config)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // Resolve game references from database
    crate::plugin::sync::resolve_game_references(&app_state.db, app_state.db_backend, &mut records)
        .await;

    // Process records: sign those with sign=true
    let signer = app_state.attestation_signer.as_deref();
    let processor = SyncProcessor::new(signer, user_did.to_string());
    let processed = processor
        .process_records(records)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let processed_count = processed.len();

    // Write processed records to user's PDS
    let write_results = pds_write::write_records_to_pds(&app_state, user_did, processed).await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "processed": processed_count,
        "written": write_results.len()
    })))
}

async fn unlink(
    State(app_state): State<AppState>,
    Path(plugin_id): Path<String>,
    claims: Claims,
) -> Result<Json<serde_json::Value>, AppError> {
    let user_did = claims.did();

    // Delete tokens
    let deleted = tokens::delete_tokens(&app_state.db, app_state.db_backend, user_did, &plugin_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // TODO: Delete accountLink record from user's PDS

    Ok(Json(serde_json::json!({
        "status": "ok",
        "was_linked": deleted
    })))
}

async fn load_plugin_secrets(
    db: &sqlx::Pool<sqlx::Any>,
    db_backend: crate::db::DatabaseBackend,
    encryption_key: Option<&[u8; 32]>,
    plugin_id: &str,
) -> HashMap<String, String> {
    use crate::plugin::encryption::decrypt;
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

    // Try to load from database first (if encryption key is available)
    if let Some(key) = encryption_key {
        let sql = crate::db::adapt_sql(
            "SELECT config FROM happyview_plugin_configs WHERE plugin_id = ?",
            db_backend,
        );

        if let Ok(Some((config_json,))) = sqlx::query_as::<_, (String,)>(&sql)
            .bind(plugin_id)
            .fetch_optional(db)
            .await
            && let Ok(config) = serde_json::from_str::<serde_json::Value>(&config_json)
            && let Some(secrets_obj) = config.get("secrets").and_then(|s| s.as_object())
        {
            // DB keys are full env var names (e.g., PLUGIN_STEAM_API_KEY)
            // Strip prefix to get short names for plugin (e.g., API_KEY)
            let prefix = format!("PLUGIN_{}_", plugin_id.to_uppercase());
            let db_secrets: HashMap<String, String> = secrets_obj
                .iter()
                .filter_map(|(k, v)| {
                    v.as_str().and_then(|encrypted_b64| {
                        // Decode base64 and decrypt
                        let encrypted = BASE64.decode(encrypted_b64).ok()?;
                        let decrypted = decrypt(key, &encrypted).ok()?;
                        let value = String::from_utf8(decrypted).ok()?;
                        // Strip prefix from key to get short name
                        let short_key = k.strip_prefix(&prefix).unwrap_or(k).to_string();
                        Some((short_key, value))
                    })
                })
                .collect();

            if !db_secrets.is_empty() {
                return db_secrets;
            }
        }
    }

    // Fall back to environment variables
    let prefix = format!("PLUGIN_{}_", plugin_id.to_uppercase());
    std::env::vars()
        .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|name| (name.to_string(), v)))
        .collect()
}
