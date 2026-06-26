pub(crate) mod procedure;
pub(crate) mod query;

use axum::Json;
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
use crate::rate_limit::CheckResult;
use crate::resolve::resolve_nsid_authority;

/// Parse a raw query string into a map where repeated keys become JSON arrays.
/// Single-value keys remain as JSON strings for backward compatibility.
pub(crate) fn parse_query_params(query: &str) -> HashMap<String, Value> {
    let mut multi: HashMap<String, Vec<String>> = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (
                urlencoding::decode(k).unwrap_or_default().into_owned(),
                urlencoding::decode(v).unwrap_or_default().into_owned(),
            ),
            None => (
                urlencoding::decode(pair).unwrap_or_default().into_owned(),
                String::new(),
            ),
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
        return Err(AppError::PdsError(status, bytes));
    }

    Ok(Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Body::from(bytes))
        .unwrap())
}

/// Find the client key from claims, headers, or query params.
///
/// Authenticated requests (claims present) must provide one — returns Err
/// if missing.  Anonymous requests fall back to `"anonymous"`.
fn extract_client_key(
    claims: Option<&Claims>,
    parts: &Parts,
    query_params: &std::collections::HashMap<String, serde_json::Value>,
) -> Result<String, AppError> {
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
        Some(k) => Ok(k),
        None if claims.is_some() => Err(AppError::Auth(
            "Missing client identification. Provide an X-Client-Key header or client_key query parameter.".into(),
        )),
        None => Ok("anonymous".to_string()),
    }
}

/// Resolve the client key and run origin/secret validation.
fn resolve_client_key(
    state: &AppState,
    claims: Option<&Claims>,
    parts: &Parts,
    query_params: &std::collections::HashMap<String, serde_json::Value>,
) -> Result<String, AppError> {
    let client_key = extract_client_key(claims, parts, query_params)?;

    // Log validation warnings but always return the key for rate limiting.
    if !state.rate_limiter.is_valid_client_key(&client_key) {
        tracing::warn!("Unknown client key: {client_key}");
        return Ok(client_key);
    }

    // If the key came from a session cookie, it was already validated at login time.
    let from_session = claims
        .and_then(|c| c.client_key())
        .map(|k| k == client_key)
        .unwrap_or(false);

    if !from_session {
        let origin = parts.headers.get("origin").and_then(|v| v.to_str().ok());

        if let Some(origin) = origin {
            if !state
                .rate_limiter
                .validate_client_origin(&client_key, origin)
            {
                tracing::warn!("Origin mismatch for client {client_key}: got {origin}");
            }
        } else {
            let secret = parts
                .headers
                .get("x-client-secret")
                .and_then(|v| v.to_str().ok());

            match secret {
                Some(s) if state.rate_limiter.validate_client_secret(&client_key, s) => {}
                Some(_) => {
                    tracing::warn!("Invalid client secret for {client_key}");
                }
                None => {
                    tracing::warn!("No Origin or X-Client-Secret for client {client_key}");
                }
            }
        }
    }

    Ok(client_key)
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
        resolve_client_key(&state, claims.as_ref(), &parts, &params)?
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
            if !state.proxy_config.load().allows(&method) {
                return Err(AppError::Forbidden(
                    "NSID not allowed by proxy policy".into(),
                ));
            }
            let mut response = proxy_to_authority(&state, &method, &raw_query, None).await?;
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
pub async fn xrpc_post(
    State(state): State<AppState>,
    Path(method): Path<String>,
    RawQuery(raw_query): RawQuery,
    xrpc_claims: XrpcClaims,
    parts: Parts,
    Json(body): Json<serde_json::Value>,
) -> Result<Response, AppError> {
    let raw_query = raw_query.unwrap_or_default();
    let mut params = parse_query_params(&raw_query);
    let claims = xrpc_claims.identity;

    let rate_key = if let Some(sa) = &xrpc_claims.service_auth {
        format!("service:{}", sa.did)
    } else {
        resolve_client_key(&state, claims.as_ref(), &parts, &params)?
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
            if !state.proxy_config.load().allows(&method) {
                return Err(AppError::Forbidden(
                    "NSID not allowed by proxy policy".into(),
                ));
            }
            let mut response = proxy_to_authority(&state, &method, &raw_query, Some(&body)).await?;
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

    if let Some(ref param_schema) = lexicon.parameters {
        coerce_params(&mut params, param_schema);
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
    fn anonymous_request_gets_anonymous_rate_key() {
        let parts = empty_parts();
        let params = HashMap::new();
        let result = extract_client_key(None, &parts, &params);
        assert_eq!(result.unwrap(), "anonymous");
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
        assert_eq!(result.unwrap(), "hvc_abc");
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
        assert_eq!(result.unwrap(), "hvc_from_header");
    }

    #[test]
    fn client_key_from_query_params() {
        let parts = empty_parts();
        let mut params = HashMap::new();
        params.insert("client_key".into(), json!("hvc_from_query"));
        let result = extract_client_key(None, &parts, &params);
        assert_eq!(result.unwrap(), "hvc_from_query");
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
        assert_eq!(result.unwrap(), "hvc_fallback");
    }
}
