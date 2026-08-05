use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptErrorType {
    Syntax,
    Runtime,
    Timeout,
    MissingHandle,
}

impl std::fmt::Display for ScriptErrorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScriptErrorType::Syntax => write!(f, "syntax"),
            ScriptErrorType::Runtime => write!(f, "runtime"),
            ScriptErrorType::Timeout => write!(f, "timeout"),
            ScriptErrorType::MissingHandle => write!(f, "missing_handle"),
        }
    }
}

/// Parse a Lua error message to extract a line number.
///
/// Prefix used to tag auth errors that pass through the Lua runtime boundary.
/// The script executor checks for this prefix to recover `AppError::Auth` from
/// a generic `mlua::Error::runtime`, returning 401 instead of 500.
pub const LUA_AUTH_ERROR_PREFIX: &str = "AUTH_ERROR:";

/// mlua errors look like:
/// - `[string "..."]:42: attempt to index a nil value`
/// - `runtime error: [string "..."]:10: bad argument`
///
/// Returns `(Some(line), cleaned_message)` or `(None, original_message)`.
pub fn parse_lua_line(raw: &str) -> (Option<u32>, String) {
    if let Some(bracket_pos) = raw.find("]:") {
        let after_bracket = &raw[bracket_pos + 2..];
        if let Some(colon_pos) = after_bracket.find(": ") {
            let line_str = &after_bracket[..colon_pos];
            if let Ok(line) = line_str.parse::<u32>() {
                let message = after_bracket[colon_pos + 2..].to_string();
                return (Some(line), message);
            }
        }
    }
    (None, raw.to_string())
}

/// Render an error together with its full `source()` chain.
///
/// `reqwest::Error`'s `Display` deliberately omits its source, so a transport
/// failure formats as nothing more than `error sending request for url (…)` —
/// leaving NXDOMAIN, TLS failure, connection refused and timeout completely
/// indistinguishable in the logs. That is exactly the information needed to
/// tell "this host is gone" from "we are misconfigured", so walk the chain and
/// append each cause.
pub fn describe_error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        // Chains frequently restate the outer message; don't repeat it.
        if !out.ends_with(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = cause.source();
    }
    out
}

#[derive(Debug)]
pub enum AppError {
    Auth(String),
    /// Auth failure with a DPoP nonce that the client should retry with.
    AuthDpopNonce(String),
    BadGateway(String),
    BadRequest(String),
    Conflict(String),
    FeatureDisabled(String),
    Forbidden(String),
    InsufficientPermissions(String),
    Internal(String),
    NotFound(String),
    PdsError(StatusCode, Bytes),
    /// The instance is misconfigured (e.g. an insecure `SESSION_SECRET`); the
    /// requested auth path is disabled until an operator fixes it. Renders as
    /// 503 so clients and the dashboard can distinguish it from a normal 401.
    ServerMisconfigured(String),
    RateLimited {
        retry_after: u64,
        limit: u32,
        reset: u64,
    },
    ScriptError {
        error_type: ScriptErrorType,
        message: String,
        method: String,
        line: Option<u32>,
    },
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::Auth(msg) => write!(f, "auth error: {msg}"),
            AppError::AuthDpopNonce(nonce) => write!(f, "auth error: use_dpop_nonce ({nonce})"),
            AppError::BadGateway(msg) => write!(f, "bad gateway: {msg}"),
            AppError::BadRequest(msg) => write!(f, "bad request: {msg}"),
            AppError::Conflict(msg) => write!(f, "conflict: {msg}"),
            AppError::FeatureDisabled(msg) => write!(f, "feature disabled: {msg}"),
            AppError::Forbidden(msg) => write!(f, "forbidden: {msg}"),
            AppError::InsufficientPermissions(perm) => write!(f, "Missing permission: {perm}"),
            AppError::Internal(msg) => write!(f, "internal error: {msg}"),
            AppError::NotFound(msg) => write!(f, "not found: {msg}"),
            AppError::PdsError(status, _) => write!(f, "PDS error: {status}"),
            AppError::ServerMisconfigured(msg) => write!(f, "server misconfigured: {msg}"),
            AppError::RateLimited { retry_after, .. } => {
                write!(f, "rate limited: retry after {retry_after}s")
            }
            AppError::ScriptError {
                error_type,
                message,
                method,
                line,
            } => {
                if let Some(l) = line {
                    write!(
                        f,
                        "script {error_type} error in {method} at line {l}: {message}"
                    )
                } else {
                    write!(f, "script {error_type} error in {method}: {message}")
                }
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::PdsError(status, body) => (
                status,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response(),
            AppError::AuthDpopNonce(nonce) => {
                let body = serde_json::json!({ "error": "use_dpop_nonce", "dpop_nonce": nonce });
                let mut response = (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response();
                if let Ok(val) = axum::http::HeaderValue::from_str(&nonce) {
                    response.headers_mut().insert("dpop-nonce", val);
                }
                response
            }
            AppError::ScriptError {
                error_type,
                message,
                method,
                line,
            } => {
                let status = match &error_type {
                    ScriptErrorType::Timeout => StatusCode::REQUEST_TIMEOUT,
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                tracing::error!(%method, ?error_type, ?line, "{message}");
                let body = serde_json::json!({
                    "error": "script_error",
                    "errorType": error_type,
                    "message": message,
                    "method": method,
                    "line": line,
                });
                (status, axum::Json(body)).into_response()
            }
            AppError::FeatureDisabled(msg) => {
                let body = serde_json::json!({
                    "error": "FeatureDisabled",
                    "message": msg,
                });
                (StatusCode::NOT_FOUND, axum::Json(body)).into_response()
            }
            AppError::InsufficientPermissions(perm) => {
                let body = serde_json::json!({
                    "error": "InsufficientPermissions",
                    "message": format!("Missing permission: {perm}"),
                });
                (StatusCode::FORBIDDEN, axum::Json(body)).into_response()
            }
            AppError::ServerMisconfigured(msg) => {
                let body = serde_json::json!({
                    "error": "ServerMisconfigured",
                    "message": msg,
                });
                (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body)).into_response()
            }
            AppError::Internal(msg) => {
                // Never leak internal details (SQL/driver errors, decryption
                // failures, crypto/config state) to clients. Log the real message
                // server-side with a correlation id and return only that id so an
                // operator can find it in the logs.
                let correlation_id = format!("{:016x}", rand::random::<u64>());
                tracing::error!(correlation_id, "internal error: {msg}");
                let body = serde_json::json!({
                    "error": "Internal server error",
                    "correlationId": correlation_id,
                });
                (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(body)).into_response()
            }
            AppError::RateLimited {
                retry_after,
                limit,
                reset,
            } => {
                let body = serde_json::json!({
                    "error": "RateLimited",
                    "message": "Too many requests",
                });
                let mut response =
                    (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response();
                let headers = response.headers_mut();
                headers.insert("RateLimit-Limit", limit.into());
                headers.insert("RateLimit-Remaining", 0u32.into());
                headers.insert("RateLimit-Reset", reset.into());
                headers.insert("Retry-After", retry_after.into());
                response
            }
            other => {
                let (status, message) = match &other {
                    AppError::Auth(msg) => (StatusCode::UNAUTHORIZED, msg.clone()),
                    AppError::BadGateway(msg) => (StatusCode::BAD_GATEWAY, msg.clone()),
                    AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
                    AppError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
                    AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
                    AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
                    AppError::PdsError(..)
                    | AppError::AuthDpopNonce(..)
                    | AppError::FeatureDisabled(..)
                    | AppError::InsufficientPermissions(..)
                    | AppError::Internal(..)
                    | AppError::ServerMisconfigured(..)
                    | AppError::RateLimited { .. }
                    | AppError::ScriptError { .. } => unreachable!(),
                };

                let body = serde_json::json!({ "error": message });
                (status, axum::Json(body)).into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;

    async fn response_parts(err: AppError) -> (StatusCode, serde_json::Value) {
        let resp = err.into_response();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        (status, json)
    }

    #[tokio::test]
    async fn server_misconfigured_returns_503() {
        let (status, body) = response_parts(AppError::ServerMisconfigured(
            "SESSION_SECRET is not set".into(),
        ))
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "ServerMisconfigured");
        assert_eq!(body["message"], "SESSION_SECRET is not set");
    }

    #[tokio::test]
    async fn auth_error_returns_401() {
        let (status, body) = response_parts(AppError::Auth("bad token".into())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "bad token");
    }

    #[tokio::test]
    async fn bad_request_returns_400() {
        let (status, body) = response_parts(AppError::BadRequest("missing field".into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "missing field");
    }

    #[tokio::test]
    async fn internal_error_returns_500_without_leaking_details() {
        let (status, body) =
            response_parts(AppError::Internal("secret SQL driver details".into())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        // The raw internal message must not appear anywhere in the response body.
        assert_eq!(body["error"], "Internal server error");
        assert!(
            !body.to_string().contains("secret SQL driver details"),
            "internal error details must not be leaked to the client"
        );
        // A correlation id is returned so the operator can find the real error in logs.
        assert!(
            body["correlationId"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "expected a non-empty correlationId"
        );
    }

    #[tokio::test]
    async fn script_error_returns_500_with_structured_body() {
        let (status, body) = response_parts(AppError::ScriptError {
            error_type: ScriptErrorType::Runtime,
            message: "attempt to index a nil value".into(),
            method: "games.gamesgamesgamesgames.search".into(),
            line: Some(42),
        })
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "script_error");
        assert_eq!(body["errorType"], "runtime");
        assert_eq!(body["message"], "attempt to index a nil value");
        assert_eq!(body["method"], "games.gamesgamesgamesgames.search");
        assert_eq!(body["line"], 42);
    }

    #[tokio::test]
    async fn script_error_timeout_returns_408() {
        let (status, body) = response_parts(AppError::ScriptError {
            error_type: ScriptErrorType::Timeout,
            message: "script exceeded execution time limit".into(),
            method: "test.method".into(),
            line: None,
        })
        .await;
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(body["error"], "script_error");
        assert_eq!(body["errorType"], "timeout");
        assert!(body["line"].is_null());
    }

    #[tokio::test]
    async fn script_error_syntax_returns_500() {
        let (status, body) = response_parts(AppError::ScriptError {
            error_type: ScriptErrorType::Syntax,
            message: "unexpected symbol near ')'".into(),
            method: "test.method".into(),
            line: Some(5),
        })
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "script_error");
        assert_eq!(body["errorType"], "syntax");
        assert_eq!(body["line"], 5);
    }

    #[tokio::test]
    async fn feature_disabled_returns_404() {
        let (status, body) =
            response_parts(AppError::FeatureDisabled("spaces not enabled".into())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "FeatureDisabled");
        assert_eq!(body["message"], "spaces not enabled");
    }

    #[tokio::test]
    async fn not_found_returns_404() {
        let (status, body) = response_parts(AppError::NotFound("no such thing".into())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "no such thing");
    }

    #[tokio::test]
    async fn pds_error_preserves_status_and_body() {
        let raw_body = Bytes::from(r#"{"error":"upstream"}"#);
        let resp = AppError::PdsError(StatusCode::BAD_GATEWAY, raw_body.clone()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body, raw_body);
    }

    #[derive(Debug)]
    struct TestError {
        message: &'static str,
        cause: Option<Box<TestError>>,
    }

    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.message)
        }
    }

    impl std::error::Error for TestError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.cause
                .as_ref()
                .map(|c| c.as_ref() as &(dyn std::error::Error + 'static))
        }
    }

    #[test]
    fn describe_error_chain_includes_every_cause() {
        // The shape reqwest actually produces: a useless outer message wrapping
        // the cause that says what really happened.
        let err = TestError {
            message: "error sending request for url (https://hackrlabs.dev/.well-known/did.json)",
            cause: Some(Box::new(TestError {
                message: "client error (Connect)",
                cause: Some(Box::new(TestError {
                    message: "dns error: failed to lookup address information",
                    cause: None,
                })),
            })),
        };

        let rendered = describe_error_chain(&err);
        assert!(
            rendered.contains("dns error: failed to lookup address information"),
            "the root cause must survive rendering, got: {rendered}"
        );
        assert!(rendered.contains("client error (Connect)"));
        assert!(rendered.starts_with("error sending request for url"));
    }

    #[test]
    fn describe_error_chain_handles_no_source() {
        let err = TestError {
            message: "standalone failure",
            cause: None,
        };
        assert_eq!(describe_error_chain(&err), "standalone failure");
    }

    #[test]
    fn describe_error_chain_does_not_repeat_restated_causes() {
        // Some error types embed their cause's text in their own Display.
        let err = TestError {
            message: "outer: inner detail",
            cause: Some(Box::new(TestError {
                message: "inner detail",
                cause: None,
            })),
        };
        assert_eq!(describe_error_chain(&err), "outer: inner detail");
    }

    #[test]
    fn parse_lua_line_extracts_line_number() {
        let (line, msg) = parse_lua_line("[string \"...\"]:42: attempt to index a nil value");
        assert_eq!(line, Some(42));
        assert_eq!(msg, "attempt to index a nil value");
    }

    #[test]
    fn parse_lua_line_no_line_number() {
        let (line, msg) = parse_lua_line("some other error");
        assert_eq!(line, None);
        assert_eq!(msg, "some other error");
    }

    #[test]
    fn parse_lua_line_runtime_error_prefix() {
        let (line, msg) = parse_lua_line("runtime error: [string \"...\"]:10: bad argument");
        assert_eq!(line, Some(10));
        assert_eq!(msg, "bad argument");
    }

    #[test]
    fn script_error_type_serializes() {
        assert_eq!(
            serde_json::to_string(&ScriptErrorType::Syntax).unwrap(),
            "\"syntax\""
        );
        assert_eq!(
            serde_json::to_string(&ScriptErrorType::Runtime).unwrap(),
            "\"runtime\""
        );
        assert_eq!(
            serde_json::to_string(&ScriptErrorType::Timeout).unwrap(),
            "\"timeout\""
        );
        assert_eq!(
            serde_json::to_string(&ScriptErrorType::MissingHandle).unwrap(),
            "\"missing_handle\""
        );
    }

    #[test]
    fn display_formats() {
        assert_eq!(AppError::Auth("x".into()).to_string(), "auth error: x");
        assert_eq!(
            AppError::BadRequest("y".into()).to_string(),
            "bad request: y"
        );
        assert_eq!(
            AppError::Internal("z".into()).to_string(),
            "internal error: z"
        );
        assert_eq!(
            AppError::FeatureDisabled("x".into()).to_string(),
            "feature disabled: x"
        );
        assert_eq!(AppError::NotFound("w".into()).to_string(), "not found: w");
        assert_eq!(
            AppError::PdsError(StatusCode::BAD_GATEWAY, Bytes::new()).to_string(),
            "PDS error: 502 Bad Gateway"
        );
        assert_eq!(
            AppError::ScriptError {
                error_type: ScriptErrorType::Runtime,
                message: "oops".into(),
                method: "test.method".into(),
                line: Some(5),
            }
            .to_string(),
            "script runtime error in test.method at line 5: oops"
        );
    }
}
