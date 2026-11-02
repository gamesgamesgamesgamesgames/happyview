use axum::extract::{DefaultBodyLimit, State};
use axum::http::{Method, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use tower_http::catch_panic::CatchPanicLayer;
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
    let domain_routes = Router::new()
        .merge(
            crate::spaces::routes::space_routes().layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::feature_middleware::require_spaces,
            )),
        )
        // Public and unauthenticated: detection happens before authorization,
        // and must also answer on a build with spaces disabled.
        .merge(crate::spaces::describe::describe_routes())
        .merge(crate::spaces::simplespace::simplespace_routes().layer(
            axum::middleware::from_fn_with_state(
                state.clone(),
                crate::feature_middleware::require_spaces,
            ),
        ))
        .nest("/auth", crate::auth::routes::routes())
        .route(
            "/auth/linked-repo/start",
            get(crate::linked_repos::flow::start_handler),
        )
        .route(
            "/auth/linked-repo/info",
            get(crate::linked_repos::flow::invite_info_handler),
        )
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

    let app_routes = crate::dashboard::attach(
        Router::new()
            .nest("/admin", admin::admin_routes(state.clone()))
            .merge(domain_routes)
            .route("/", get(|| async { Redirect::to("/dashboard/") })),
        &state.config,
    );

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
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::csrf::guard,
        ))
        .layer(CatchPanicLayer::custom(panic_response))
        .layer(TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn_with_state(state.clone(), cors))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::telemetry_middleware::count_http_bytes,
        ))
        .with_state(state)
}

/// A handler that panics answers as an internal error does: a logged detail and
/// a generic body. Left uncaught, the panic drops the connection with no
/// response at all.
fn panic_response(panic: Box<dyn std::any::Any + Send + 'static>) -> axum::response::Response {
    let detail = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic payload");
    AppError::Internal(format!("handler panicked: {detail}")).into_response()
}

/// Allowed request methods, shared by both CORS policies.
const CORS_ALLOW_METHODS: &str = "GET, POST, DELETE, OPTIONS";

/// Headers a credentialed (first-party, cookie-bearing) request may send.
const CORS_ALLOW_HEADERS_CREDENTIALED: &str = "content-type, authorization, cookie, x-client-key, x-client-secret, dpop, \
     atproto-accept-labelers, atproto-proxy";

/// Headers a credential-less (third-party, cookieless DPoP) request may send.
/// Identical to the credentialed set minus `cookie`.
const CORS_ALLOW_HEADERS_ANON: &str = "content-type, authorization, x-client-key, x-client-secret, dpop, \
     atproto-accept-labelers, atproto-proxy";

/// Cross-Origin Resource Sharing policy.
///
/// This deliberately replaces a single permissive `CorsLayer`. The old policy
/// reflected *any* `Origin` **and** allowed credentials, which let a malicious
/// page drive the admin API with the victim's cookie and read the response
/// (finding C2). Instead we apply two policies keyed on trust:
///
/// - **Trusted first-party origins** — those in the [`DomainCache`] (the domains
///   HappyView actually serves the dashboard/admin UI on) — get their origin
///   reflected *with* `Access-Control-Allow-Credentials: true`, so the
///   cookie-authenticated dashboard works cross-origin if ever hosted on a
///   second registered domain.
/// - **Any other origin** — e.g. a third-party app or an attacker page — gets a
///   credential-*less* grant: its origin is reflected but credentials are never
///   allowed. Third-party clients authenticate with explicit DPoP + client-key
///   headers (never ambient cookies), so they keep working; an attacker page
///   can neither ride the admin cookie nor read a credentialed response.
///
/// The one rule that must never be violated: reflecting an arbitrary origin and
/// allowing credentials at the same time.
async fn cors(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // No `Origin` header → not a CORS request (same-origin navigation,
    // server-to-server, curl). Emit no CORS headers at all.
    let Some(origin) = origin else {
        return next.run(req).await;
    };

    let credentialed = state.domain_cache.is_allowed_origin(&origin).await;

    let is_preflight = req.method() == Method::OPTIONS
        && req
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);

    let mut cors_headers = header::HeaderMap::new();
    if let Ok(value) = header::HeaderValue::from_str(&origin) {
        cors_headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    } else {
        // Malformed origin — refuse it entirely rather than emit a broken header.
        if is_preflight {
            return preflight_response(cors_headers);
        }
        return next.run(req).await;
    }
    cors_headers.insert(header::VARY, header::HeaderValue::from_static("origin"));
    if credentialed {
        cors_headers.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            header::HeaderValue::from_static("true"),
        );
    }

    if is_preflight {
        cors_headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            header::HeaderValue::from_static(CORS_ALLOW_METHODS),
        );
        cors_headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            header::HeaderValue::from_static(if credentialed {
                CORS_ALLOW_HEADERS_CREDENTIALED
            } else {
                CORS_ALLOW_HEADERS_ANON
            }),
        );
        cors_headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            header::HeaderValue::from_static("86400"),
        );
        return preflight_response(cors_headers);
    }

    let mut resp = next.run(req).await;
    resp.headers_mut().extend(cors_headers);
    resp
}

/// Build a `204 No Content` preflight response carrying the given CORS headers.
fn preflight_response(cors_headers: header::HeaderMap) -> Response {
    let mut resp = Response::new(axum::body::Body::empty());
    *resp.status_mut() = axum::http::StatusCode::NO_CONTENT;
    resp.headers_mut().extend(cors_headers);
    resp
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

    let version: &str = crate::version::version();

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
        // Whether a managed-hosting platform administers this instance, so the
        // dashboard can say so.
        "platform_managed": state.config.platform_managed(),
        "logo_url": logo_url,
        // The key hpy signs in with; see `auth::cli_client`.
        "cli_client_key": crate::auth::cli_client::CLIENT_KEY,
        "features": {
            "spaces": spaces_enabled,
        },
        // Startup configuration problems (e.g. an insecure SESSION_SECRET) so the
        // dashboard can surface them to an operator. Empty when healthy.
        "configErrors": state.config.config_errors(),
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

    let base_scopes = crate::linked_repos::scope::client_base_scopes(
        oauth_client.client_metadata.scope.as_deref(),
        &oauth_client.client_metadata.client_id,
    );
    match crate::linked_repos::db::all_scopes(&state).await {
        Ok(grant_scopes) => {
            let union = crate::linked_repos::scope::union(&grant_scopes, &base_scopes);
            metadata["scope"] = serde_json::Value::String(union);
        }
        Err(e) => {
            // Serving a doc without grant scopes would break every linked-repo
            // authorization; log loudly rather than failing silently.
            tracing::error!(error = %e, "failed to compute linked-repo scope union");
        }
    }

    // Point at the key set rather than inlining it. Authorization servers
    // cache this document; one that meets a signature by a key it has not
    // seen can refetch a `jwks_uri` (Tranquil does, at most once a minute)
    // but cannot refresh an inline set until the whole document expires.
    apply_jwks_uri(&mut metadata, &domain_url);

    Json(metadata)
}

/// Swap a confidential client's inline `jwks` for the `jwks_uri` that
/// serves the instance's published keys (`oauth::routes::client_jwks`):
/// `current`, `pending` and `retiring`. A public client has neither.
fn apply_jwks_uri(metadata: &mut serde_json::Value, domain_url: &str) {
    let Some(fields) = metadata.as_object_mut() else {
        return;
    };
    if fields.remove("jwks").is_none() {
        return;
    }
    fields.insert(
        "jwks_uri".into(),
        serde_json::Value::String(format!(
            "{}/oauth/clients/{}/jwks.json",
            domain_url.trim_end_matches('/'),
            crate::oauth::client_keys::INSTANCE_OWNER
        )),
    );
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

    let signing_key = p256::ecdsa::SigningKey::from_slice(private_bytes.as_slice())
        .map_err(|e| AppError::Internal(format!("invalid signing key: {e}")))?;
    let public_key = signing_key.verifying_key();
    let compressed = public_key.to_sec1_point(true);

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

#[cfg(test)]
mod tests {
    use super::apply_jwks_uri;

    #[test]
    fn a_public_clients_metadata_gets_no_key_set() {
        let mut metadata = serde_json::json!({
            "client_id": "https://example.com/oauth-client-metadata.json",
            "client_name": "Example",
        });

        apply_jwks_uri(&mut metadata, "https://example.com");

        assert!(metadata.get("jwks").is_none());
        assert!(metadata.get("jwks_uri").is_none());
    }

    #[test]
    fn a_confidential_clients_inline_key_set_becomes_a_jwks_uri() {
        let mut metadata = serde_json::json!({
            "client_id": "https://example.com/base/oauth-client-metadata.json",
            // atrium's single-key projection.
            "jwks": {"keys": []},
        });

        apply_jwks_uri(&mut metadata, "https://example.com/base/");

        assert!(metadata.get("jwks").is_none(), "{metadata}");
        assert_eq!(
            metadata["jwks_uri"],
            "https://example.com/base/oauth/clients/instance/jwks.json"
        );
    }
}
