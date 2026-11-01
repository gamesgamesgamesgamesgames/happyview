pub(crate) mod local;
pub(crate) mod procedure;
pub(crate) mod query;
pub(crate) mod scope_check;
pub(crate) mod validate;

use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::Response;
use serde_json::Value;
use std::collections::HashMap;

use crate::AppState;
use crate::auth::{Claims, XrpcClaims};
use crate::error::AppError;
use crate::lexicon::LexiconType;
use crate::rate_limit::{CheckResult, RateLimiter};
use crate::resolve::resolve_nsid_authority;

/// Decode one query-string component.
///
/// A query string is form-urlencoded, which the URL standard requires of the
/// query and which `URLSearchParams`, a browser `fetch` and most HTTP clients
/// all produce -- so a space arrives as `+`, not `%20`. Percent-decoding alone
/// left it as a literal plus, and a search for `Integration Demo` therefore
/// looked for `Integration+Demo` and matched nothing.
///
/// The replacement runs first and a real plus survives it: `a+b` is read as
/// `a b`, while an encoded one is `a%2Bb`, carries no plus to replace, and
/// percent-decodes to `a+b` afterwards.
fn decode_component(raw: &str) -> String {
    urlencoding::decode(&raw.replace('+', " "))
        .unwrap_or_default()
        .into_owned()
}

/// Parse a raw query string into a map where repeated keys become JSON arrays.
/// Single-value keys remain as JSON strings for backward compatibility.
pub(crate) fn parse_query_params(query: &str) -> HashMap<String, Value> {
    let mut multi: HashMap<String, Vec<String>> = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (decode_component(k), decode_component(v)),
            None => (decode_component(pair), String::new()),
        };
        multi.entry(key).or_default().push(value);
    }
    multi
        .into_iter()
        .map(|(k, v)| {
            if v.len() == 1 {
                (k, Value::String(v.into_iter().next().unwrap()))
            } else {
                (k, Value::Array(v.into_iter().map(Value::String).collect()))
            }
        })
        .collect()
}

/// Coerce query-param values from strings to their lexicon-declared types.
///
/// HTTP query params arrive as strings. Without this, Lua scripts receive
/// `"25"` (a string) for `params.limit`, which Postgres rejects when used
/// in LIMIT (`argument of LIMIT must be type bigint, not type text`).
pub(crate) fn coerce_params(params: &mut HashMap<String, Value>, parameters: &Value) {
    let properties = match parameters.get("properties").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return,
    };
    for (key, schema) in properties {
        let type_str = match schema.get("type").and_then(|t| t.as_str()) {
            Some(t) => t,
            None => continue,
        };
        let Some(val) = params.get(key) else {
            continue;
        };
        let Some(s) = val.as_str() else {
            continue;
        };
        match type_str {
            "integer" => {
                if let Ok(n) = s.parse::<i64>() {
                    params.insert(key.clone(), Value::Number(n.into()));
                }
            }
            "boolean" => match s {
                "true" | "1" => {
                    params.insert(key.clone(), Value::Bool(true));
                }
                "false" | "0" => {
                    params.insert(key.clone(), Value::Bool(false));
                }
                _ => {}
            },
            "number" => {
                if let Ok(n) = s.parse::<f64>()
                    && let Some(num) = serde_json::Number::from_f64(n)
                {
                    params.insert(key.clone(), Value::Number(num));
                }
            }
            _ => {}
        }
    }
}

/// Headers forwarded to the caller's PDS when service-proxying.
///
/// `atproto-proxy` names the destination the PDS should relay to; HappyView
/// must not resolve it itself, because the inter-service token that
/// accompanies a relayed request is signed by the *user's* identity key, which
/// lives on the PDS. Resolving the DID here and posting directly would produce
/// an unauthenticated request that looks like it worked in testing.
const FORWARDED_HEADERS: [&str; 2] = ["atproto-proxy", "atproto-accept-labelers"];

fn forwarded_headers(parts: &Parts) -> Vec<(String, String)> {
    FORWARDED_HEADERS
        .iter()
        .filter_map(|name| {
            parts
                .headers
                .get(*name)
                .and_then(|v| v.to_str().ok())
                .map(|v| ((*name).to_string(), v.to_string()))
        })
        .collect()
}

/// Reject a request that asks to write to somebody else's repo.
///
/// A forwarded write always acts as the caller: the DPoP session is looked up
/// *by DID*, and the cookie path presents the caller's own token. A `repo` that
/// names anyone else can therefore only fail — and it used to fail deep in
/// session lookup, reported as a problem with the caller's login rather than
/// with the repo they asked for. That message cost a reporter several days
/// (lexicon issue #14); this is the same trap one layer up, so it is refused
/// here with the answer instead.
fn check_repo_matches_caller(body: &Value, caller_did: &str) -> Result<(), AppError> {
    let Some(repo) = body.get("repo").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    if repo == caller_did {
        return Ok(());
    }
    Err(AppError::BadRequest(format!(
        "cannot write to {repo}: a proxied request acts as the authenticated user \
         ({caller_did}), so it can only reach their own repo. Writing to another \
         account's repo needs an admin-managed grant — see linked repos."
    )))
}

/// Forward an unrecognized XRPC method to the caller's own PDS, as the caller.
///
/// This is the AT Protocol service proxying model: absent an `atproto-proxy`
/// header the PDS handles the request itself, and with one it relays onwards.
/// Either way the destination is a function of *who is asking*, not of the
/// NSID — which is the distinction [`proxy_to_authority`] gets wrong.
async fn forward_to_caller_pds(
    state: &AppState,
    claims: &Claims,
    method: &str,
    body: &Value,
    parts: &Parts,
) -> Result<Response, AppError> {
    check_repo_matches_caller(body, claims.did())?;

    let auth = crate::repo::pds_auth_for_claims(state, claims).await?;
    let headers = forwarded_headers(parts);

    enforce_granted_scopes(state, &auth, method, body, &headers).await?;

    tracing::debug!(
        %method,
        did = %claims.did(),
        proxied = !headers.is_empty(),
        "XRPC service proxy: forwarding to caller's PDS"
    );

    let resp = auth
        .post_json_with_headers(state, claims.did(), method, body, &headers)
        .await?;

    crate::repo::forward_pds_response(resp).await
}

/// Check a forwarded request against the scopes the session actually holds.
///
/// A local pre-flight, not the security boundary — the PDS enforces the token's
/// scopes regardless. Its value is closing the gap CLAUDE.md records for
/// `pds::call`: "fine where PDSes enforce granular scopes and a hole where they
/// don't". It only covers operations that map unambiguously; see
/// [`scope_check`] for why that is deliberate.
async fn enforce_granted_scopes(
    state: &AppState,
    auth: &crate::repo::PdsAuth,
    method: &str,
    body: &Value,
    headers: &[(String, String)],
) -> Result<(), AppError> {
    let proxy_aud = headers
        .iter()
        .find(|(name, _)| name == "atproto-proxy")
        .map(|(_, value)| value.as_str());

    let required = scope_check::required_for(method, body, proxy_aud);
    if required.is_empty() {
        return Ok(());
    }

    // `None` means the granted scopes are not knowable from here (the cookie
    // path), not that none were granted. Skipping is correct: the PDS is the
    // authority and still enforces them.
    let Some(scopes) = auth.granted_scopes(state).await? else {
        return Ok(());
    };

    let granted = happyview_scopes::ScopePermissions::parse(&scopes);
    scope_check::check(&granted, &required, method)
}

/// Forward an unrecognized query to the caller's own PDS, as the caller.
///
/// The GET counterpart of [`forward_to_caller_pds`]. There is no `repo` check
/// here because a query has no body to name one, and repo *reads* are public.
async fn forward_query_to_caller_pds(
    state: &AppState,
    claims: &Claims,
    method: &str,
    query: &str,
    parts: &Parts,
) -> Result<Response, AppError> {
    let auth = crate::repo::pds_auth_for_claims(state, claims).await?;
    let headers = forwarded_headers(parts);

    // Repo reads need no scope — there is no read permission in the grammar,
    // because records are public. A *proxied* query is a different matter: it
    // is an rpc call to the named audience, and `required_for` maps it.
    enforce_granted_scopes(state, &auth, method, &Value::Null, &headers).await?;

    tracing::debug!(
        %method,
        did = %claims.did(),
        proxied = !headers.is_empty(),
        "XRPC service proxy: forwarding query to caller's PDS"
    );

    let resp = auth
        .get_with_headers(state, claims.did(), method, query, &headers)
        .await?;

    crate::repo::forward_pds_response(resp).await
}

/// Proxy an unrecognized XRPC method to its home AppView resolved via DNS.
pub(crate) async fn proxy_to_authority(
    state: &AppState,
    method: &str,
    query_string: &str,
    body: Option<&serde_json::Value>,
) -> Result<Response, AppError> {
    let (_did, pds_endpoint) = resolve_nsid_authority(&state.http, &state.config.plc_url, method)
        .await
        .map_err(|e| {
            AppError::BadGateway(format!("failed to resolve authority for {method}: {e}"))
        })?;

    let mut url = format!("{}/xrpc/{method}", pds_endpoint.trim_end_matches('/'),);
    if !query_string.is_empty() {
        url.push('?');
        url.push_str(query_string);
    }

    let request = if let Some(json_body) = body {
        state.http.post(&url).json(json_body)
    } else {
        state.http.get(&url)
    };

    let upstream = request
        .send()
        .await
        .map_err(|e| AppError::BadGateway(format!("upstream request failed for {method}: {e}")))?;

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    let bytes = upstream.bytes().await.map_err(|e| {
        AppError::BadGateway(format!(
            "failed to read upstream response for {method}: {e}"
        ))
    })?;

    if !status.is_success() {
        // The upstream body is relayed verbatim by `AppError::PdsError`, so
        // without this the caller sees a foreign error with no record on our
        // side of where it came from — which is exactly how an anonymous
        // misroute reads as a HappyView bug to whoever reported it.
        tracing::warn!(
            %method,
            %pds_endpoint,
            %status,
            body = %String::from_utf8_lossy(&bytes),
            "XRPC proxy: upstream returned an error"
        );
        return Err(AppError::PdsError(status, bytes));
    }

    tracing::debug!(%method, %pds_endpoint, "XRPC proxy: resolved authority");

    Ok(Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Body::from(bytes))
        .unwrap())
}

/// The rate key of a request that names no client.
const ANONYMOUS_CLIENT_KEY: &str = "anonymous";

/// Find the client key from claims, headers, or query params.
///
/// Authenticated requests (claims present) must provide one — returns Err
/// if missing. An anonymous request that names none is `None`.
fn extract_client_key(
    claims: Option<&Claims>,
    parts: &Parts,
    query_params: &std::collections::HashMap<String, serde_json::Value>,
) -> Result<Option<String>, AppError> {
    let found = claims
        .and_then(|c| c.client_key().map(|k| k.to_string()))
        .or_else(|| {
            parts
                .headers
                .get("x-client-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            query_params
                .get("client_key")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    match found {
        Some(k) => Ok(Some(k)),
        None if claims.is_some() => Err(AppError::Auth(
            "Missing client identification. Provide an X-Client-Key header or client_key query parameter.".into(),
        )),
        None => Ok(None),
    }
}

/// The rate key of a request: the client it names, once that client is
/// proven, and `anonymous` otherwise.
///
/// A client is proven by the session it signed in through, by its registered
/// Origin, or by one of its live secrets. A key that is unknown, or arrives
/// with the wrong Origin, a wrong secret or neither, is warned about and
/// limited as `anonymous`: the key is public, so taking it on its word would
/// let any caller spend that client's bucket. A request that names no client
/// is the ordinary case for a public query and logs nothing.
fn resolve_client_key(
    rate_limiter: &RateLimiter,
    claims: Option<&Claims>,
    parts: &Parts,
    query_params: &std::collections::HashMap<String, serde_json::Value>,
    secret_used: impl FnOnce(String),
) -> Result<String, AppError> {
    let Some(client_key) = extract_client_key(claims, parts, query_params)? else {
        return Ok(ANONYMOUS_CLIENT_KEY.to_string());
    };

    if !rate_limiter.is_valid_client_key(&client_key) {
        tracing::warn!("Unknown client key: {client_key}");
        return Ok(ANONYMOUS_CLIENT_KEY.to_string());
    }

    // If the key came from a session cookie, it was already validated at login time.
    let from_session = claims
        .and_then(|c| c.client_key())
        .map(|k| k == client_key)
        .unwrap_or(false);

    if !from_session {
        let origin = parts.headers.get("origin").and_then(|v| v.to_str().ok());

        if let Some(origin) = origin {
            if !rate_limiter.validate_client_origin(&client_key, origin) {
                tracing::warn!("Origin mismatch for client {client_key}: got {origin}");
                return Ok(ANONYMOUS_CLIENT_KEY.to_string());
            }
        } else {
            let secret = parts
                .headers
                .get("x-client-secret")
                .and_then(|v| v.to_str().ok());

            match secret.map(|s| rate_limiter.match_client_secret(&client_key, s)) {
                Some(Some(secret_id)) => {
                    if rate_limiter.note_secret_use(&secret_id) {
                        secret_used(secret_id);
                    }
                }
                Some(None) => {
                    tracing::warn!("Invalid client secret for {client_key}");
                    return Ok(ANONYMOUS_CLIENT_KEY.to_string());
                }
                None => {
                    tracing::warn!("No Origin or X-Client-Secret for client {client_key}");
                    return Ok(ANONYMOUS_CLIENT_KEY.to_string());
                }
            }
        }
    }

    Ok(client_key)
}

/// Record, off the request path, that a client secret authenticated a request.
fn record_secret_use(state: &AppState, secret_id: String) {
    let (db, backend) = (state.db.clone(), state.db_backend);
    tokio::spawn(async move {
        crate::oauth::client_secrets::record_use(&db, backend, &secret_id).await;
    });
}

/// Apply rate limit headers to a response.
fn apply_rate_limit_headers(response: &mut Response, remaining: u32, limit: u32, reset: u64) {
    let headers = response.headers_mut();
    headers.insert("RateLimit-Limit", limit.into());
    headers.insert("RateLimit-Remaining", remaining.into());
    headers.insert("RateLimit-Reset", reset.into());
}

/// Catch-all GET handler for XRPC queries.
pub async fn xrpc_get(
    State(state): State<AppState>,
    Path(method): Path<String>,
    RawQuery(raw_query): RawQuery,
    xrpc_claims: XrpcClaims,
    parts: Parts,
) -> Result<Response, AppError> {
    let raw_query = raw_query.unwrap_or_default();
    let mut params = parse_query_params(&raw_query);
    let identity_claims = xrpc_claims.identity;

    // For service auth, synthesise Claims from the caller's DID so the
    // query handler has an identity to work with.
    let service_auth_claims_owned;
    let claims: Option<Claims> = if let Some(ref sa) = xrpc_claims.service_auth {
        let has_access = crate::service_entries::check_access(
            &state.db,
            state.db_backend,
            &sa.aud_fragment,
            &method,
        )
        .await?;

        if !has_access {
            crate::event_log::log_event(
                &state.db,
                crate::event_log::EventLog {
                    event_type: "service_auth.access_denied".to_string(),
                    severity: crate::event_log::Severity::Error,
                    actor_did: Some(sa.did.clone()),
                    subject: Some(method.clone()),
                    detail: serde_json::json!({
                        "fragment": sa.aud_fragment,
                        "reason": "service entry not authorized for this XRPC"
                    }),
                },
                state.db_backend,
            )
            .await;

            return Err(AppError::Auth(format!(
                "service '{}' is not authorized for '{}'",
                sa.aud_fragment, method
            )));
        }

        service_auth_claims_owned = Claims::internal(sa.did.clone());
        Some(service_auth_claims_owned)
    } else {
        identity_claims
    };

    let rate_key = if let Some(sa) = &xrpc_claims.service_auth {
        format!("service:{}", sa.did)
    } else {
        resolve_client_key(
            &state.rate_limiter,
            claims.as_ref(),
            &parts,
            &params,
            |id| record_secret_use(&state, id),
        )?
    };

    let lexicon = state.lexicons.get(&method).await;

    // Determine token cost: per-NSID override → type default → 1
    let cost = if let Some(ref lex) = lexicon {
        lex.token_cost.unwrap_or_else(|| {
            let type_str = format!("{:?}", lex.lexicon_type).to_lowercase();
            state
                .rate_limiter
                .default_cost_for_type(&rate_key, &type_str)
        })
    } else {
        state.rate_limiter.default_cost_for_type(&rate_key, "proxy")
    };

    let check = state.rate_limiter.check(&rate_key, cost);

    match check {
        CheckResult::Limited {
            retry_after,
            limit,
            reset,
        } => {
            return Err(AppError::RateLimited {
                retry_after,
                limit,
                reset,
            });
        }
        CheckResult::Allowed { .. } | CheckResult::Disabled => {}
    }

    let lexicon = match lexicon {
        Some(l) => l,
        None => {
            let proxy_config = state.proxy_config.load();
            if !proxy_config.allows(&method) {
                return Err(AppError::Forbidden(
                    "NSID not allowed by proxy policy".into(),
                ));
            }

            let mut response = match proxy_config.routing {
                crate::proxy_config::ProxyRouting::ServiceProxy => {
                    // Queries, unlike procedures, may arrive anonymously. Under
                    // this routing there is no PDS to send an anonymous request
                    // to — the destination is a function of who is asking. This
                    // is the deliberate break that makes the mode opt-in.
                    let claims = claims.as_ref().ok_or_else(|| {
                        AppError::Auth(format!(
                            "{method} is not implemented by this AppView, and forwarding it \
                             requires authentication: under service-proxy routing a query is \
                             sent to the caller's own PDS."
                        ))
                    })?;
                    forward_query_to_caller_pds(&state, claims, &method, &raw_query, &parts).await?
                }
                crate::proxy_config::ProxyRouting::Authority => {
                    proxy_to_authority(&state, &method, &raw_query, None).await?
                }
            };

            if let CheckResult::Allowed {
                remaining,
                limit,
                reset,
            } = check
            {
                apply_rate_limit_headers(&mut response, remaining, limit, reset);
            }
            return Ok(response);
        }
    };

    if lexicon.lexicon_type != LexiconType::Query {
        return Err(AppError::BadRequest(format!(
            "{method} is not a query endpoint"
        )));
    }

    if let Some(ref param_schema) = lexicon.parameters {
        coerce_params(&mut params, param_schema);
        validate::params(&mut params, param_schema)?;
    }

    let mut response =
        query::handle_query(&state, &method, &params, &lexicon, claims.as_ref()).await?;
    if let CheckResult::Allowed {
        remaining,
        limit,
        reset,
    } = check
    {
        apply_rate_limit_headers(&mut response, remaining, limit, reset);
    }
    Ok(response)
}

/// Catch-all POST handler for XRPC procedures.
/// A JSON body, read as strictly as the `Json` extractor did: the media type
/// has to say JSON, and the bytes have to parse.
fn json_body(parts: &Parts, raw: &axum::body::Bytes) -> Result<serde_json::Value, AppError> {
    let declared = parts
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let mime = declared.split(';').next().unwrap_or("").trim();
    if !(mime.eq_ignore_ascii_case("application/json")
        || mime.to_ascii_lowercase().ends_with("+json"))
    {
        return Err(AppError::BadRequest(
            "this method expects a JSON body; set content-type: application/json".into(),
        ));
    }

    serde_json::from_slice(raw)
        .map_err(|e| AppError::BadRequest(format!("request body is not valid JSON: {e}")))
}

/// Store a byte body and describe it to the script as a blob ref.
///
/// The media type recorded is the caller's, since `*/*` means the caller
/// decides; a lexicon that pins one supplies it when the caller does not.
async fn ingest_body(
    state: &AppState,
    parts: &Parts,
    lexicon: &crate::lexicon::ParsedLexicon,
    raw: &axum::body::Bytes,
) -> Result<serde_json::Value, AppError> {
    if raw.is_empty() {
        return Err(AppError::BadRequest(
            "this method expects a body of bytes, and none was sent".into(),
        ));
    }

    let declared = parts
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or("").trim().to_string())
        .filter(|value| !value.is_empty());

    let mime = match (declared, crate::script::encoding::of_input(lexicon)) {
        (Some(sent), _) => sent,
        (None, crate::script::Encoding::Fixed(pinned)) => pinned,
        (None, _) => "application/octet-stream".to_string(),
    };

    let cid = crate::blobs::put(&state.db, state.db_backend, raw, &mime).await?;
    serde_json::to_value(crate::blobs::BlobRef::new(cid, mime, raw.len() as i64))
        .map_err(|e| AppError::Internal(format!("failed to describe a stored blob: {e}")))
}

pub async fn xrpc_post(
    State(state): State<AppState>,
    Path(method): Path<String>,
    RawQuery(raw_query): RawQuery,
    xrpc_claims: XrpcClaims,
    parts: Parts,
    // Bytes rather than `Json`, because whether this body is JSON at all is
    // the lexicon's to declare: `uploadBlob` takes `*/*`, and an extractor
    // chosen before the method is known cannot honour that. A method whose
    // lexicon says JSON is parsed exactly as strictly as before.
    raw_body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let raw_query = raw_query.unwrap_or_default();
    let mut params = parse_query_params(&raw_query);
    let claims = xrpc_claims.identity;

    let rate_key = if let Some(sa) = &xrpc_claims.service_auth {
        format!("service:{}", sa.did)
    } else {
        resolve_client_key(
            &state.rate_limiter,
            claims.as_ref(),
            &parts,
            &params,
            |id| record_secret_use(&state, id),
        )?
    };

    if claims.is_none()
        && xrpc_claims.space_credential.is_none()
        && xrpc_claims.service_auth.is_none()
    {
        return Err(AppError::Auth(
            "XRPC procedures require DPoP authentication".into(),
        ));
    }

    let lexicon = state.lexicons.get(&method).await;

    // Determine token cost: per-NSID override → type default → 1
    let cost = if let Some(ref lex) = lexicon {
        lex.token_cost.unwrap_or_else(|| {
            let type_str = format!("{:?}", lex.lexicon_type).to_lowercase();
            state
                .rate_limiter
                .default_cost_for_type(&rate_key, &type_str)
        })
    } else {
        state.rate_limiter.default_cost_for_type(&rate_key, "proxy")
    };

    let check = state.rate_limiter.check(&rate_key, cost);

    match check {
        CheckResult::Limited {
            retry_after,
            limit,
            reset,
        } => {
            return Err(AppError::RateLimited {
                retry_after,
                limit,
                reset,
            });
        }
        CheckResult::Allowed { .. } | CheckResult::Disabled => {}
    }

    let lexicon = match lexicon {
        Some(l) => l,
        None => {
            let proxy_config = state.proxy_config.load();
            if !proxy_config.allows(&method) {
                return Err(AppError::Forbidden(
                    "NSID not allowed by proxy policy".into(),
                ));
            }

            let mut response = match proxy_config.routing {
                // Every caller here is authenticated — `xrpc_post` refuses
                // anonymous procedures above — so there is always a PDS to
                // forward to.
                crate::proxy_config::ProxyRouting::ServiceProxy => {
                    let claims = claims.as_ref().ok_or_else(|| {
                        AppError::Auth("XRPC procedures require DPoP authentication".into())
                    })?;
                    forward_to_caller_pds(
                        &state,
                        claims,
                        &method,
                        &json_body(&parts, &raw_body)?,
                        &parts,
                    )
                    .await?
                }
                crate::proxy_config::ProxyRouting::Authority => {
                    proxy_to_authority(
                        &state,
                        &method,
                        &raw_query,
                        Some(&json_body(&parts, &raw_body)?),
                    )
                    .await?
                }
            };

            if let CheckResult::Allowed {
                remaining,
                limit,
                reset,
            } = check
            {
                apply_rate_limit_headers(&mut response, remaining, limit, reset);
            }
            return Ok(response);
        }
    };

    if lexicon.lexicon_type != LexiconType::Procedure {
        return Err(AppError::BadRequest(format!(
            "{method} is not a procedure endpoint"
        )));
    }

    // For service auth, synthesise Claims from the caller's DID so the
    // procedure handler has an identity to work with.
    let service_auth_claims_owned;
    let (claims, sa_ref) = if let Some(ref sa) = xrpc_claims.service_auth {
        service_auth_claims_owned = Claims::internal(sa.did.clone());
        (&service_auth_claims_owned, Some(sa))
    } else {
        let c = claims
            .ok_or_else(|| AppError::Auth("XRPC procedures require DPoP authentication".into()))?;
        // Re-bind to a reference with matching lifetime
        service_auth_claims_owned = c;
        (&service_auth_claims_owned, None)
    };

    // After auth, so a caller who may not call the procedure learns nothing
    // about its lexicon from a validation error.
    if let Some(ref param_schema) = lexicon.parameters {
        coerce_params(&mut params, param_schema);
        validate::params(&mut params, param_schema)?;
    }

    // `input.encoding` decides what the script receives. Bytes are stored
    // before the script runs and reach it as an ordinary blob ref, so a script
    // authorizes and records a body without ever carrying it — which is the
    // same two-step `uploadBlob` and `createRecord` already describe.
    let body = if crate::script::encoding::of_input(&lexicon).is_bytes() {
        ingest_body(&state, &parts, &lexicon, &raw_body).await?
    } else {
        let body = json_body(&parts, &raw_body)?;
        validate::input(&state, &lexicon, &body).await?;
        body
    };

    let mut response =
        procedure::handle_procedure(&state, &method, claims, &body, &params, &lexicon, sa_ref)
            .await?;
    if let CheckResult::Allowed {
        remaining,
        limit,
        reset,
    } = check
    {
        apply_rate_limit_headers(&mut response, remaining, limit, reset);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // parse_query_params
    // -----------------------------------------------------------------------

    #[test]
    fn parse_query_params_single_values() {
        let params = parse_query_params("limit=10&cursor=abc");
        assert_eq!(params.get("limit").unwrap(), "10");
        assert_eq!(params.get("cursor").unwrap(), "abc");
    }

    #[test]
    fn parse_query_params_repeated_key_becomes_array() {
        let params = parse_query_params("tag=a&tag=b&tag=c");
        let arr = params.get("tag").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0], "a");
        assert_eq!(arr[1], "b");
        assert_eq!(arr[2], "c");
    }

    /// A space reaches us as `+`, because that is what form-urlencoding --
    /// and therefore `URLSearchParams` and every browser `fetch` -- sends. A
    /// plus that was meant literally arrives as `%2B` and must survive.
    #[test]
    fn parse_query_params_reads_plus_as_a_space() {
        let params = parse_query_params("q=Integration+Demo&tag=c%2B%2B&both=a+b%2Bc");

        assert_eq!(params["q"], json!("Integration Demo"));
        assert_eq!(params["tag"], json!("c++"));
        assert_eq!(params["both"], json!("a b+c"));
    }

    /// Keys are form-urlencoded too.
    #[test]
    fn parse_query_params_reads_plus_in_a_key() {
        let params = parse_query_params("a+key=value");

        assert_eq!(params["a key"], json!("value"));
    }

    #[test]
    fn parse_query_params_empty_string() {
        let params = parse_query_params("");
        assert!(params.is_empty());
    }

    #[test]
    fn parse_query_params_url_decodes() {
        let params = parse_query_params("uri=at%3A%2F%2Fdid%3Aplc%3Aabc%2Fcol%2Frkey");
        assert_eq!(params.get("uri").unwrap(), "at://did:plc:abc/col/rkey");
    }

    #[test]
    fn parse_query_params_key_without_value() {
        let params = parse_query_params("flag");
        assert_eq!(params.get("flag").unwrap(), "");
    }

    // -----------------------------------------------------------------------
    // coerce_params
    // -----------------------------------------------------------------------

    fn make_schema(properties: Value) -> Value {
        json!({ "properties": properties })
    }

    #[test]
    fn coerce_integer_from_string() {
        let mut params = HashMap::new();
        params.insert("limit".into(), Value::String("25".into()));
        let schema = make_schema(json!({ "limit": { "type": "integer" } }));
        coerce_params(&mut params, &schema);
        assert_eq!(params["limit"], json!(25));
    }

    #[test]
    fn coerce_boolean_true_variants() {
        for val in &["true", "1"] {
            let mut params = HashMap::new();
            params.insert("active".into(), Value::String((*val).into()));
            let schema = make_schema(json!({ "active": { "type": "boolean" } }));
            coerce_params(&mut params, &schema);
            assert_eq!(params["active"], json!(true), "failed for input {val}");
        }
    }

    #[test]
    fn coerce_boolean_false_variants() {
        for val in &["false", "0"] {
            let mut params = HashMap::new();
            params.insert("active".into(), Value::String((*val).into()));
            let schema = make_schema(json!({ "active": { "type": "boolean" } }));
            coerce_params(&mut params, &schema);
            assert_eq!(params["active"], json!(false), "failed for input {val}");
        }
    }

    #[test]
    fn coerce_number_from_string() {
        let mut params = HashMap::new();
        params.insert(
            "score".into(),
            Value::String(std::f64::consts::PI.to_string()),
        );
        let schema = make_schema(json!({ "score": { "type": "number" } }));
        coerce_params(&mut params, &schema);
        assert_eq!(params["score"], json!(std::f64::consts::PI));
    }

    #[test]
    fn coerce_leaves_string_type_alone() {
        let mut params = HashMap::new();
        params.insert("name".into(), Value::String("42".into()));
        let schema = make_schema(json!({ "name": { "type": "string" } }));
        coerce_params(&mut params, &schema);
        assert_eq!(params["name"], json!("42"));
    }

    #[test]
    fn coerce_skips_params_not_in_schema() {
        let mut params = HashMap::new();
        params.insert("extra".into(), Value::String("100".into()));
        let schema = make_schema(json!({ "limit": { "type": "integer" } }));
        coerce_params(&mut params, &schema);
        assert_eq!(params["extra"], json!("100"));
    }

    #[test]
    fn coerce_skips_invalid_integer() {
        let mut params = HashMap::new();
        params.insert("limit".into(), Value::String("not_a_number".into()));
        let schema = make_schema(json!({ "limit": { "type": "integer" } }));
        coerce_params(&mut params, &schema);
        assert_eq!(params["limit"], json!("not_a_number"));
    }

    #[test]
    fn coerce_skips_already_typed_value() {
        let mut params = HashMap::new();
        params.insert("limit".into(), json!(25));
        let schema = make_schema(json!({ "limit": { "type": "integer" } }));
        coerce_params(&mut params, &schema);
        assert_eq!(params["limit"], json!(25));
    }

    #[test]
    fn coerce_empty_schema_is_noop() {
        let mut params = HashMap::new();
        params.insert("limit".into(), Value::String("25".into()));
        let schema = json!({});
        coerce_params(&mut params, &schema);
        assert_eq!(params["limit"], json!("25"));
    }

    // -----------------------------------------------------------------------
    // extract_client_key
    // -----------------------------------------------------------------------

    fn empty_parts() -> axum::http::request::Parts {
        let (parts, _) = axum::http::Request::builder()
            .uri("/xrpc/test")
            .body(())
            .unwrap()
            .into_parts();
        parts
    }

    #[test]
    fn anonymous_request_names_no_client_key() {
        let parts = empty_parts();
        let params = HashMap::new();
        let result = extract_client_key(None, &parts, &params);
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn authenticated_request_without_client_key_is_rejected() {
        let parts = empty_parts();
        let params = HashMap::new();
        let claims = crate::auth::Claims::new_for_test("did:plc:test".into());
        let result = extract_client_key(Some(&claims), &parts, &params);
        assert!(result.is_err());
    }

    #[test]
    fn authenticated_request_with_client_key_in_claims() {
        let parts = empty_parts();
        let params = HashMap::new();
        let claims = crate::auth::Claims::with_client_key("did:plc:test".into(), "hvc_abc".into());
        let result = extract_client_key(Some(&claims), &parts, &params);
        assert_eq!(result.unwrap().as_deref(), Some("hvc_abc"));
    }

    #[test]
    fn x_client_key_header_used_for_anonymous() {
        let (parts, _) = axum::http::Request::builder()
            .uri("/xrpc/test")
            .header("x-client-key", "hvc_from_header")
            .body(())
            .unwrap()
            .into_parts();
        let params = HashMap::new();
        let result = extract_client_key(None, &parts, &params);
        assert_eq!(result.unwrap().as_deref(), Some("hvc_from_header"));
    }

    #[test]
    fn client_key_from_query_params() {
        let parts = empty_parts();
        let mut params = HashMap::new();
        params.insert("client_key".into(), json!("hvc_from_query"));
        let result = extract_client_key(None, &parts, &params);
        assert_eq!(result.unwrap().as_deref(), Some("hvc_from_query"));
    }

    // -----------------------------------------------------------------------
    // resolve_client_key
    // -----------------------------------------------------------------------

    #[derive(Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn rate_limiter() -> std::sync::Arc<crate::rate_limit::RateLimiter> {
        crate::rate_limit::RateLimiter::new(crate::rate_limit::RateLimitDefaults {
            query_cost: 1,
            procedure_cost: 1,
            proxy_cost: 1,
        })
    }

    /// What `resolve_client_key` answers for `parts`, and what it logged at
    /// warn level or above.
    fn resolve_logging_warnings(parts: &Parts) -> (String, String) {
        resolve_against(&rate_limiter(), parts)
    }

    fn resolve_against(
        rate_limiter: &crate::rate_limit::RateLimiter,
        parts: &Parts,
    ) -> (String, String) {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let key = tracing::subscriber::with_default(subscriber, || {
            resolve_client_key(rate_limiter, None, parts, &HashMap::new(), |_| {}).unwrap()
        });
        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        (key, log)
    }

    #[test]
    fn an_anonymous_request_is_rate_limited_as_anonymous_and_warns_about_nothing() {
        let (key, log) = resolve_logging_warnings(&empty_parts());
        assert_eq!(key, "anonymous");
        assert_eq!(log, "", "an anonymous request logged at warn level");
    }

    #[test]
    fn a_client_key_nobody_registered_is_warned_about_and_limited_as_anonymous() {
        for named in ["hvc_nobody", "anonymous"] {
            let (parts, _) = axum::http::Request::builder()
                .uri("/xrpc/test")
                .header("x-client-key", named)
                .body(())
                .unwrap()
                .into_parts();
            let (key, log) = resolve_logging_warnings(&parts);
            assert_eq!(key, "anonymous");
            assert!(
                log.contains(&format!("Unknown client key: {named}")),
                "got: {log}"
            );
        }
    }

    const VICTIM: &str = "hvc_victim";
    const VICTIM_URI: &str = "https://victim.example";
    const VICTIM_SECRET: &str = "hvs_victim_secret";

    /// A limiter knowing one client, with a bucket of `capacity` that does
    /// not refill within a test.
    fn limiter_with_victim(capacity: u32) -> std::sync::Arc<crate::rate_limit::RateLimiter> {
        let rate_limiter = rate_limiter();
        rate_limiter.register_client_identity(
            VICTIM.into(),
            crate::rate_limit::ClientIdentity {
                secrets: vec![crate::rate_limit::SecretHash {
                    id: "secret-1".into(),
                    hash: crate::oauth::client_secrets::hash(VICTIM_SECRET),
                }],
                client_uri: VICTIM_URI.into(),
            },
        );
        rate_limiter.register_client_config(
            VICTIM.into(),
            crate::rate_limit::RateLimitConfig {
                capacity,
                refill_rate: 0.000_001,
                default_query_cost: 1,
                default_procedure_cost: 1,
                default_proxy_cost: 1,
            },
        );
        rate_limiter
    }

    #[test]
    fn a_registered_key_without_proof_is_limited_as_anonymous() {
        let rate_limiter = limiter_with_victim(10);
        for (case, headers, warning) in [
            (
                "the wrong Origin",
                vec![
                    ("x-client-key", VICTIM),
                    ("origin", "https://attacker.example"),
                ],
                "Origin mismatch",
            ),
            (
                "a wrong secret",
                vec![("x-client-key", VICTIM), ("x-client-secret", "hvs_guess")],
                "Invalid client secret",
            ),
            (
                "neither",
                vec![("x-client-key", VICTIM)],
                "No Origin or X-Client-Secret",
            ),
        ] {
            let (key, log) = resolve_against(&rate_limiter, &parts_with(&headers));
            assert_eq!(key, "anonymous", "{case}");
            assert!(log.contains(warning), "{case}: got {log}");
        }
    }

    #[test]
    fn a_registered_key_with_proof_is_limited_as_itself() {
        let rate_limiter = limiter_with_victim(10);
        for (case, headers) in [
            (
                "its Origin",
                vec![("x-client-key", VICTIM), ("origin", VICTIM_URI)],
            ),
            (
                "its secret",
                vec![("x-client-key", VICTIM), ("x-client-secret", VICTIM_SECRET)],
            ),
        ] {
            let (key, log) = resolve_against(&rate_limiter, &parts_with(&headers));
            assert_eq!(key, VICTIM, "{case}");
            assert_eq!(log, "", "{case}");
        }

        let claims = crate::auth::Claims::with_client_key("did:plc:test".into(), VICTIM.into());
        let key = resolve_client_key(
            &rate_limiter,
            Some(&claims),
            &parts_with(&[]),
            &HashMap::new(),
            |_| {},
        )
        .unwrap();
        assert_eq!(key, VICTIM, "its session");
    }

    /// The end of the chain: requests naming a client they cannot prove
    /// leave that client's bucket untouched.
    #[test]
    fn naming_another_client_does_not_spend_its_bucket() {
        let rate_limiter = limiter_with_victim(1);
        let forged = parts_with(&[("x-client-key", VICTIM), ("x-client-secret", "hvs_guess")]);
        for _ in 0..5 {
            let (key, _) = resolve_against(&rate_limiter, &forged);
            rate_limiter.check(&key, 1);
        }
        assert!(matches!(
            rate_limiter.check(VICTIM, 1),
            crate::rate_limit::CheckResult::Allowed { .. }
        ));
    }

    // -----------------------------------------------------------------------
    // service proxying
    // -----------------------------------------------------------------------

    #[test]
    fn repo_matching_the_caller_is_accepted() {
        let body = json!({ "repo": "did:plc:caller", "collection": "com.example.post" });
        assert!(check_repo_matches_caller(&body, "did:plc:caller").is_ok());
    }

    #[test]
    fn a_body_with_no_repo_is_accepted() {
        let body = json!({ "collection": "com.example.post" });
        assert!(check_repo_matches_caller(&body, "did:plc:caller").is_ok());
    }

    /// The refusal has to name the way forward, not just say no — the failure
    /// it replaces sent a reporter looking at their own OAuth setup for days.
    #[test]
    fn another_accounts_repo_is_refused_and_says_where_to_go() {
        let body = json!({ "repo": "did:plc:someone-else" });
        let err = check_repo_matches_caller(&body, "did:plc:caller").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("did:plc:someone-else"), "{message}");
        assert!(message.contains("did:plc:caller"), "{message}");
        assert!(message.contains("linked repos"), "{message}");
    }

    fn parts_with(headers: &[(&str, &str)]) -> axum::http::request::Parts {
        let mut builder = axum::http::Request::builder().uri("/xrpc/test");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let (parts, _) = builder.body(()).unwrap().into_parts();
        parts
    }

    #[test]
    fn only_the_named_headers_are_forwarded() {
        let parts = parts_with(&[
            ("atproto-proxy", "did:web:api.bsky.app#bsky_appview"),
            ("atproto-accept-labelers", "did:plc:abc"),
            // Must not be relayed: the PDS issues its own credentials for the
            // forwarded request, and ours are bound to this hop.
            ("authorization", "DPoP secret"),
            ("dpop", "proof"),
            ("cookie", "session=secret"),
            ("x-client-secret", "shh"),
        ]);

        let forwarded = forwarded_headers(&parts);
        assert_eq!(forwarded.len(), 2);
        assert_eq!(
            forwarded[0],
            (
                "atproto-proxy".to_string(),
                "did:web:api.bsky.app#bsky_appview".to_string()
            )
        );
        assert_eq!(
            forwarded[1],
            (
                "atproto-accept-labelers".to_string(),
                "did:plc:abc".to_string()
            )
        );
    }

    #[test]
    fn no_forwarded_headers_when_none_are_present() {
        assert!(forwarded_headers(&parts_with(&[])).is_empty());
    }

    #[test]
    fn authenticated_request_uses_header_when_claims_lack_key() {
        let (parts, _) = axum::http::Request::builder()
            .uri("/xrpc/test")
            .header("x-client-key", "hvc_fallback")
            .body(())
            .unwrap()
            .into_parts();
        let params = HashMap::new();
        let claims = crate::auth::Claims::new_for_test("did:plc:test".into());
        let result = extract_client_key(Some(&claims), &parts, &params);
        assert_eq!(result.unwrap().as_deref(), Some("hvc_fallback"));
    }
}
