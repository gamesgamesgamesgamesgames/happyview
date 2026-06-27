use axum::extract::{DefaultBodyLimit, State};
use axum::http::{Method, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use bytes::Bytes;
use http_body_util::Full;
use std::convert::Infallible;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::AppState;
use crate::admin;
use crate::auth::XrpcClaims;
use crate::domain_middleware::resolve_domain;
use crate::error::AppError;
use crate::profile;
use crate::rate_limit::CheckResult;
use crate::repo;
use crate::xrpc;

pub fn router(state: AppState) -> Router {
    let static_dir = state.config.static_dir.clone();

    // SPA fallback: when ServeDir can't find a static file, check if the
    // parent path contains a _/index.html (Next.js dynamic route shell)
    // before falling back to the root index.html.
    let fallback_dir = static_dir.clone();
    let spa_fallback = tower::service_fn(move |req: axum::http::Request<_>| {
        let dir = fallback_dir.clone();
        async move {
            let path = req.uri().path();
            let segments: Vec<&str> = path.trim_matches('/').split('/').collect();

            // Try _/index.html in the parent directory (matches Next.js dynamic routes)
            if segments.len() >= 2 {
                let parent = segments[..segments.len() - 1].join("/");
                let dynamic_path = format!("{}/{}/_/index.html", dir, parent);
                if let Ok(body) = tokio::fs::read(&dynamic_path).await {
                    return Ok::<_, Infallible>(
                        axum::http::Response::builder()
                            .header("content-type", "text/html; charset=utf-8")
                            .body(Full::new(Bytes::from(body)))
                            .unwrap(),
                    );
                }
            }

            // Default: serve root index.html
            let index = format!("{}/index.html", dir);
            let body = tokio::fs::read(&index).await.unwrap_or_default();
            Ok::<_, Infallible>(
                axum::http::Response::builder()
                    .header("content-type", "text/html; charset=utf-8")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap(),
            )
        }
    });

    let serve_dir = ServeDir::new(&static_dir).not_found_service(spa_fallback);

    let domain_routes = Router::new()
        .merge(
            crate::spaces::routes::space_routes().layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::feature_middleware::require_spaces,
            )),
        )
        .merge(crate::spaces::simplespace::simplespace_routes().layer(
            axum::middleware::from_fn_with_state(
                state.clone(),
                crate::feature_middleware::require_spaces,
            ),
        ))
        .nest("/auth", crate::auth::routes::routes())
        .nest("/external-auth", crate::external_auth::routes())
        .nest("/oauth", crate::oauth::routes::routes())
        // https://atproto.com/specs/oauth#types-of-clients
        .route("/oauth-client-metadata.json", get(client_metadata))
        .route("/.well-known/did.json", get(well_known_did_json))
        .nest("/api/setup", crate::setup::routes())
        .route("/xrpc/app.bsky.actor.getProfile", get(get_profile))
        .route(
            "/xrpc/com.atproto.repo.uploadBlob",
            post(repo::upload_blob).layer(DefaultBodyLimit::max(50 * 1024 * 1024)),
        )
        .route(
            "/xrpc/dev.happyview.listApiClients",
            get(crate::dev_happyview::list_api_clients),
        )
        .route(
            "/xrpc/dev.happyview.getApiClient",
            get(crate::dev_happyview::get_api_client),
        )
        .route(
            "/xrpc/dev.happyview.createApiClient",
            post(crate::dev_happyview::create_api_client),
        )
        .route(
            "/xrpc/dev.happyview.deleteApiClient",
            post(crate::dev_happyview::delete_api_client),
        )
        // Delegation
        .route(
            "/xrpc/dev.happyview.delegation.linkAccount",
            post(crate::delegation::link_account::link_account),
        )
        .route(
            "/xrpc/dev.happyview.delegation.unlinkAccount",
            post(crate::delegation::unlink_account::unlink_account),
        )
        .route(
            "/xrpc/dev.happyview.delegation.addDelegate",
            post(crate::delegation::add_delegate::add_delegate),
        )
        .route(
            "/xrpc/dev.happyview.delegation.removeDelegate",
            post(crate::delegation::remove_delegate::remove_delegate),
        )
        .route(
            "/xrpc/dev.happyview.delegation.listAccounts",
            get(crate::delegation::list_accounts::list_accounts),
        )
        .route(
            "/xrpc/dev.happyview.delegation.getAccount",
            get(crate::delegation::get_account::get_account),
        )
        .route(
            "/xrpc/dev.happyview.delegation.listDelegates",
            get(crate::delegation::list_delegates::list_delegates),
        )
        // Catch-all for dynamically registered lexicons
        .route("/xrpc/{method}", get(xrpc::xrpc_get).post(xrpc::xrpc_post))
        .route("/config", get(config_endpoint))
        .route("/settings/logo", get(crate::admin::settings::serve_logo))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            resolve_domain,
        ));

    let app_routes = Router::new()
        .nest("/admin", admin::admin_routes(state.clone()))
        .merge(domain_routes)
        .route("/", get(|| async { Redirect::to("/dashboard/") }))
        .fallback_service(serve_dir);

    let outer = if let Some(ref base_path) = state.config.base_path {
        let bp = base_path.clone();
        let rewrite_redirects =
            axum::middleware::from_fn(move |req, next: axum::middleware::Next| {
                let bp = bp.clone();
                async move {
                    let mut response: Response = next.run(req).await;
                    if response.status().is_redirection()
                        && let Some(loc) = response.headers().get(header::LOCATION)
                        && let Ok(loc_str) = loc.to_str()
                        && loc_str.starts_with('/')
                        && !(loc_str.starts_with(&bp)
                            && (loc_str.len() == bp.len()
                                || loc_str.as_bytes().get(bp.len()) == Some(&b'/')))
                        && let Ok(new_loc) = format!("{}{}", bp, loc_str).parse()
                    {
                        response.headers_mut().insert(header::LOCATION, new_loc);
                    }
                    response
                }
            });
        Router::new()
            .route("/health", get(health))
            .nest(base_path, app_routes.layer(rewrite_redirects))
    } else {
        Router::new()
            .route("/health", get(health))
            .merge(app_routes)
    };

    outer
        .layer(TraceLayer::new_for_http())
        .layer(
            CorsLayer::new()
                .allow_origin(tower_http::cors::AllowOrigin::mirror_request())
                .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
                .allow_headers([
                    header::CONTENT_TYPE,
                    header::AUTHORIZATION,
                    header::COOKIE,
                    axum::http::HeaderName::from_static("x-client-key"),
                    axum::http::HeaderName::from_static("x-client-secret"),
                    axum::http::HeaderName::from_static("dpop"),
                    axum::http::HeaderName::from_static("atproto-accept-labelers"),
                    axum::http::HeaderName::from_static("atproto-proxy"),
                ])
                .allow_credentials(true),
        )
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn config_endpoint(
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> Json<serde_json::Value> {
    let domain_url = crate::domain_middleware::extract_domain(&req)
        .map(|d| state.config.url_with_base_path(&d.url))
        .unwrap_or_else(|| state.config.effective_public_url());

    let pool = &state.db;
    let backend = state.db_backend;

    let app_name = crate::admin::settings::get_setting(pool, "app_name", backend)
        .await
        .or_else(|| state.config.app_name.clone());

    let has_logo_data = crate::admin::settings::get_setting(pool, "logo_data", backend)
        .await
        .is_some();
    let logo_url = if has_logo_data {
        Some(format!(
            "{}/settings/logo",
            domain_url.trim_end_matches('/')
        ))
    } else {
        crate::admin::settings::get_setting(pool, "logo_uri", backend)
            .await
            .or_else(|| state.config.logo_uri.clone())
    };

    let version: &str = match option_env!("HAPPYVIEW_VERSION") {
        Some(v) if !v.is_empty() => v.trim_start_matches('v'),
        _ => env!("CARGO_PKG_VERSION"),
    };

    let spaces_enabled = crate::feature_flags::is_enabled(
        pool,
        crate::feature_flags::FeatureFlag::SPACES_ENABLED,
        backend,
    )
    .await;

    Json(serde_json::json!({
        "public_url": domain_url,
        "version": version,
        "database_backend": format!("{:?}", state.config.database_backend).to_lowercase(),
        "jetstream_url": state.config.jetstream_url,
        "relay_url": state.config.relay_url,
        "plc_url": state.config.plc_url,
        "default_rate_limit_capacity": state.config.default_rate_limit_capacity,
        "default_rate_limit_refill_rate": state.config.default_rate_limit_refill_rate,
        "app_name": app_name,
        "logo_url": logo_url,
        "features": {
            "spaces": spaces_enabled,
        },
    }))
}

async fn client_metadata(
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> Json<serde_json::Value> {
    let raw_domain_url = crate::domain_middleware::extract_domain(&req)
        .map(|d| d.url.clone())
        .unwrap_or_else(|| state.config.public_url.clone());
    let domain_url = state.config.url_with_base_path(&raw_domain_url);

    let oauth_client = state.oauth.get_for_domain(&raw_domain_url);
    let mut metadata = serde_json::to_value(&oauth_client.client_metadata).unwrap_or_default();

    // The `client_id` field in the response must exactly match the URL the
    // authorization server fetched.
    let client_id = format!(
        "{}/oauth-client-metadata.json",
        domain_url.trim_end_matches('/')
    );
    metadata["client_id"] = serde_json::Value::String(client_id);

    let pool = &state.db;
    let backend = state.db_backend;

    if let Some(name) = crate::admin::settings::get_setting(pool, "app_name", backend).await {
        metadata["client_name"] = serde_json::Value::String(name);
    }

    if let Some(uri) = crate::admin::settings::get_setting(pool, "client_uri", backend).await {
        metadata["client_uri"] = serde_json::Value::String(uri);
    }

    // Logo: prefer uploaded logo_data (served at /settings/logo), fall back to logo_uri setting
    let has_logo_data = crate::admin::settings::get_setting(pool, "logo_data", backend)
        .await
        .is_some();
    if has_logo_data {
        metadata["logo_uri"] = serde_json::Value::String(format!(
            "{}/settings/logo",
            domain_url.trim_end_matches('/')
        ));
    } else if let Some(uri) = crate::admin::settings::get_setting(pool, "logo_uri", backend).await {
        metadata["logo_uri"] = serde_json::Value::String(uri);
    }

    if let Some(uri) = crate::admin::settings::get_setting(pool, "tos_uri", backend).await {
        metadata["tos_uri"] = serde_json::Value::String(uri);
    }

    if let Some(uri) = crate::admin::settings::get_setting(pool, "policy_uri", backend).await {
        metadata["policy_uri"] = serde_json::Value::String(uri);
    }

    Json(metadata)
}

fn extract_public_key_multibase(
    identity: &crate::service_identity::ServiceIdentity,
    state: &AppState,
) -> Result<String, AppError> {
    let enc_b64 = identity
        .signing_key_enc
        .as_ref()
        .ok_or_else(|| AppError::Internal("no signing key configured".into()))?;

    let encrypted = base64::engine::general_purpose::STANDARD
        .decode(enc_b64)
        .map_err(|e| AppError::Internal(format!("invalid signing key encoding: {e}")))?;

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;

    let private_bytes = crate::plugin::encryption::decrypt(encryption_key, &encrypted)
        .map_err(|e| AppError::Internal(format!("failed to decrypt signing key: {e}")))?;

    let signing_key = p256::ecdsa::SigningKey::from_bytes(private_bytes.as_slice().into())
        .map_err(|e| AppError::Internal(format!("invalid signing key: {e}")))?;
    let public_key = signing_key.verifying_key();
    let compressed = public_key.to_encoded_point(true);

    // Multikey format: multicodec varint prefix for P-256 (0x1200) then base58btc with 'z' prefix
    let mut multikey_bytes = vec![0x80, 0x24];
    multikey_bytes.extend_from_slice(compressed.as_bytes());
    let encoded = multibase::encode(multibase::Base::Base58Btc, &multikey_bytes);
    Ok(encoded)
}

async fn well_known_did_json(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let identity = crate::service_identity::get_identity(&state.db, state.db_backend).await?;
    let identity =
        identity.ok_or_else(|| AppError::NotFound("no service identity configured".into()))?;

    if identity.mode != crate::service_identity::IdentityMode::DidWeb {
        return Err(AppError::NotFound(
            "DID document only served in did:web mode".into(),
        ));
    }

    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::BadRequest("missing Host header".into()))?;

    let entries = crate::service_entries::list_entries(&state.db, state.db_backend).await?;
    let entry_pairs: Vec<(String, String)> = entries
        .iter()
        .map(|e| (e.fragment_id.clone(), e.service_type.clone()))
        .collect();

    let extra_vms = crate::verification_methods::list_methods(&state.db, state.db_backend)
        .await?
        .into_iter()
        .map(|m| (m.fragment_id, m.key_type, m.public_key_multibase))
        .collect::<Vec<_>>();

    let service_endpoint = format!("https://{host}");

    let signing_key_multibase = extract_public_key_multibase(&identity, &state)?;

    let doc = crate::service_identity::generate_did_document(
        &identity,
        host,
        &signing_key_multibase,
        &entry_pairs,
        &service_endpoint,
        &extra_vms,
    )
    .ok_or_else(|| AppError::NotFound("DID document not available".into()))?;

    Ok(Json(doc))
}

async fn get_profile(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
) -> Result<Response, AppError> {
    let claims = xrpc_claims
        .identity
        .ok_or_else(|| AppError::Auth("getProfile requires DPoP authentication".into()))?;
    let check = if let Some(client_key) = claims.client_key() {
        let cost = state
            .rate_limiter
            .default_cost_for_type(client_key, "query");
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

    let profile =
        profile::resolve_profile(&state.http, &state.config.plc_url, claims.did()).await?;
    let mut response = Json(profile).into_response();

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
