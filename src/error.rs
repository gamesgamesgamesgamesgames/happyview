use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptErrorType {
    Syntax,
    Runtime,
    Timeout,
    /// The guest grew its linear memory past the store's ceiling. Distinct
    /// from `Runtime` because "out of memory" and "the script has a bug"
    /// call for different fixes.
    Memory,
    MissingHandle,
}

impl std::fmt::Display for ScriptErrorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScriptErrorType::Syntax => write!(f, "syntax"),
            ScriptErrorType::Runtime => write!(f, "runtime"),
            ScriptErrorType::Timeout => write!(f, "timeout"),
            ScriptErrorType::Memory => write!(f, "memory"),
            ScriptErrorType::MissingHandle => write!(f, "missing_handle"),
        }
    }
}

impl ScriptErrorType {
    pub const ALL: [ScriptErrorType; 5] = [
        ScriptErrorType::Syntax,
        ScriptErrorType::Runtime,
        ScriptErrorType::Timeout,
        ScriptErrorType::Memory,
        ScriptErrorType::MissingHandle,
    ];
}

/// An interpreter's `kind` is this type one to one; the wire enum is the
/// SDK's so a plugin cannot spell one the host does not know.
impl From<crate::plugin::ScriptErrorKind> for ScriptErrorType {
    fn from(kind: crate::plugin::ScriptErrorKind) -> Self {
        use crate::plugin::ScriptErrorKind;
        match kind {
            ScriptErrorKind::Syntax => ScriptErrorType::Syntax,
            ScriptErrorKind::Runtime => ScriptErrorType::Runtime,
            ScriptErrorKind::Timeout => ScriptErrorType::Timeout,
            ScriptErrorKind::Memory => ScriptErrorType::Memory,
            ScriptErrorKind::MissingHandle => ScriptErrorType::MissingHandle,
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
/// - `[string "script"]:42: attempt to index a nil value`
/// - `runtime error: [string "script"]:10: bad argument`
///
/// followed, when the error left a running function, by a `stack traceback:`
/// block. Returns `(Some(line), message)` or `(None, message)`, where the
/// message stops before the traceback: the traceback names only chunk lines
/// and library frames, which is noise to a client, and the raw string reaches
/// the event log untouched for anyone debugging.
pub fn parse_lua_line(raw: &str) -> (Option<u32>, String) {
    let raw = raw.split("\nstack traceback:").next().unwrap_or(raw);
    if let Some(start) = raw.find("[string \"")
        && let Some(close) = raw[start..].find("]:")
    {
        let after_bracket = &raw[start + close + 2..];
        if let Some(colon_pos) = after_bracket.find(": ") {
            let line_str = &after_bracket[..colon_pos];
            if let Ok(line) = line_str.parse::<u32>() {
                let message = after_bracket[colon_pos + 2..].to_string();
                return (Some(line), message);
            }
        }
    }
    // A file chunk: `<path>:<line>: message`, read only at the start of the
    // message (after a `runtime error: `-style category, if any). A script
    // can put `x.lua:3:` or `]:2:` anywhere in its own text, and only the
    // position Lua prepends is the location.
    let body = match raw.split_once(" error: ") {
        Some((category, rest)) if !category.contains(' ') => rest,
        _ => raw,
    };
    if let Some(colon) = body.find(':') {
        let rest = &body[colon + 1..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let location = &body[..colon];
        if digits > 0
            && rest[digits..].starts_with(": ")
            && !location.contains(' ')
            && location.ends_with(".lua")
            && let Ok(line) = rest[..digits].parse::<u32>()
        {
            return (Some(line), rest[digits + 2..].to_string());
        }
    }
    (None, raw.to_string())
}

pub use happyview_provider::http::describe_error_chain;

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
    /// A script save refused for the removed globals its body still reads.
    /// The names travel as a field of their own so a client can offer the
    /// codemod without depending on the message's wording.
    UnmigratedScript(Vec<String>),
    /// An XRPC error carrying a lexicon-defined code, e.g. `UnsupportedPolicy`.
    ///
    /// Clients switch on `error`, so a generic BadRequest is not
    /// interchangeable: "this host will not enforce that policy" and "your
    /// request was malformed" call for different handling.
    XrpcError {
        status: StatusCode,
        code: &'static str,
        message: String,
    },
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
        file: Option<String>,
    },
}

/// The envelope code a record import answers with when the storage that
/// claims the collection could not be reached. A script that passes the
/// failure on answers [`storage_unavailable`]'s 503.
pub const STORAGE_UNAVAILABLE: &str = "STORAGE_UNAVAILABLE";

/// A failed record read as the client sees it: `503 ServiceUnavailable` when
/// the store that holds the record could not be reached (`storage_down`), so
/// a client knows to retry, otherwise the `Internal` error a failed query
/// has always been.
pub fn record_read_error(e: sqlx::Error, storage_down: bool) -> AppError {
    if storage_down {
        storage_unavailable()
    } else {
        AppError::Internal(format!("DB query failed: {e}"))
    }
}

/// `503 ServiceUnavailable`: the store that holds the records could not be
/// reached, and a client can retry.
pub fn storage_unavailable() -> AppError {
    AppError::XrpcError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "ServiceUnavailable",
        message: "the record store is unavailable".to_string(),
    }
}

/// The file a failure happened in, when the chunk names one. A dashboard
/// script's chunk is `script`, which no one can open.
pub(crate) fn file_of(chunk: Option<String>) -> Option<String> {
    chunk.filter(|chunk| chunk != crate::plugin::DEFAULT_CHUNK_NAME)
}

fn unmigrated_script_message(globals: &[String]) -> String {
    crate::script::checks::unmigrated_message(globals)
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::Auth(msg) => write!(f, "auth error: {msg}"),
            AppError::XrpcError { code, message, .. } => write!(f, "{code}: {message}"),
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
            AppError::UnmigratedScript(globals) => {
                write!(f, "bad request: {}", unmigrated_script_message(globals))
            }
            AppError::RateLimited { retry_after, .. } => {
                write!(f, "rate limited: retry after {retry_after}s")
            }
            AppError::ScriptError {
                error_type,
                message,
                method,
                line,
                file,
            } => match (file, line) {
                (Some(file), Some(l)) => write!(
                    f,
                    "script {error_type} error in {method} at {file}:{l}: {message}"
                ),
                (Some(file), None) => write!(
                    f,
                    "script {error_type} error in {method} in {file}: {message}"
                ),
                (None, Some(l)) => write!(
                    f,
                    "script {error_type} error in {method} at line {l}: {message}"
                ),
                (None, None) => write!(f, "script {error_type} error in {method}: {message}"),
            },
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
                file,
            } => {
                let status = match &error_type {
                    ScriptErrorType::Timeout => StatusCode::REQUEST_TIMEOUT,
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                tracing::error!(%method, ?error_type, ?file, ?line, "{message}");
                let body = serde_json::json!({
                    "error": "script_error",
                    "errorType": error_type,
                    "message": message,
                    "method": method,
                    "line": line,
                    "file": file,
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
            AppError::UnmigratedScript(globals) => {
                let body = serde_json::json!({
                    "error": unmigrated_script_message(&globals),
                    "removed_globals": globals,
                });
                (StatusCode::BAD_REQUEST, axum::Json(body)).into_response()
            }
            AppError::XrpcError {
                status,
                code,
                message,
            } => {
                let body = serde_json::json!({
                    "error": code,
                    "message": message,
                });
                (status, axum::Json(body)).into_response()
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
                    | AppError::UnmigratedScript(..)
                    | AppError::RateLimited { .. }
                    | AppError::XrpcError { .. }
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
    async fn an_unmigrated_script_names_its_globals_in_the_message_and_as_a_field() {
        let (status, body) = response_parts(AppError::UnmigratedScript(vec![
            "db".into(),
            "params".into(),
        ]))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body,
            serde_json::json!({
                "error": "script references removed globals: db, params -- run the codemod first",
                "removed_globals": ["db", "params"],
            })
        );
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
            file: None,
        })
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "script_error");
        assert_eq!(body["errorType"], "runtime");
        assert_eq!(body["message"], "attempt to index a nil value");
        assert_eq!(body["method"], "games.gamesgamesgamesgames.search");
        assert_eq!(body["line"], 42);
        assert!(body["file"].is_null());
    }

    #[tokio::test]
    async fn script_error_timeout_returns_408() {
        let (status, body) = response_parts(AppError::ScriptError {
            error_type: ScriptErrorType::Timeout,
            message: "script exceeded execution time limit".into(),
            method: "test.method".into(),
            line: None,
            file: None,
        })
        .await;
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(body["error"], "script_error");
        assert_eq!(body["errorType"], "timeout");
        assert!(body["line"].is_null());
    }

    /// Only a timeout is the caller's 408; exhausting memory is the
    /// instance's 500, with the type on the body for the client to read.
    #[tokio::test]
    async fn script_error_memory_returns_500_with_its_type() {
        let (status, body) = response_parts(AppError::ScriptError {
            error_type: ScriptErrorType::Memory,
            message: "script exceeded its memory limit".into(),
            method: "test.method".into(),
            line: None,
            file: None,
        })
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "script_error");
        assert_eq!(body["errorType"], "memory");
    }

    #[tokio::test]
    async fn script_error_syntax_returns_500() {
        let (status, body) = response_parts(AppError::ScriptError {
            error_type: ScriptErrorType::Syntax,
            message: "unexpected symbol near ')'".into(),
            method: "test.method".into(),
            line: Some(5),
            file: None,
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

    /// A project chunk is named `@<path>`, which Lua renders as the bare
    /// path, cut from the front when it is longer than its 60-byte buffer.
    #[test]
    fn parse_lua_line_reads_a_chunk_named_by_its_path() {
        let (line, msg) =
            parse_lua_line("runtime error: src/lexicons/com/example/getThing.lua:12: boom");
        assert_eq!(line, Some(12));
        assert_eq!(msg, "boom");

        let long = "...exicons/at/happyproto/registry/somewhere/quite/deep/getArtifact.lua";
        assert!(long.len() > 60);
        let (line, msg) = parse_lua_line(&format!("{long}:3: attempt to call a nil value"));
        assert_eq!(line, Some(3));
        assert_eq!(msg, "attempt to call a nil value");

        // A message that merely contains a colon and a number is not a location.
        let (line, msg) = parse_lua_line("runtime error: expected 2:3 got nothing");
        assert_eq!(line, None);
        assert_eq!(msg, "runtime error: expected 2:3 got nothing");
    }

    #[tokio::test]
    async fn a_script_error_with_a_file_names_it_in_the_body_and_the_text() {
        let error = AppError::ScriptError {
            error_type: ScriptErrorType::Runtime,
            message: "boom".into(),
            method: "com.example.getThing".into(),
            line: Some(12),
            file: Some("src/lexicons/com/example/getThing.lua".into()),
        };
        assert_eq!(
            error.to_string(),
            "script runtime error in com.example.getThing at src/lexicons/com/example/getThing.lua:12: boom"
        );
        let (_, body) = response_parts(error).await;
        assert_eq!(body["file"], "src/lexicons/com/example/getThing.lua");
        assert_eq!(body["line"], 12);
    }

    /// Only the position Lua prepends is the location: text the script wrote
    /// into its own message must not be read as one.
    #[test]
    fn parse_lua_line_reads_only_the_leading_position() {
        let (line, msg) = parse_lua_line("src/a.lua:5: bad t[1]:2: x");
        assert_eq!(line, Some(5));
        assert_eq!(msg, "bad t[1]:2: x");

        let (line, msg) = parse_lua_line("runtime error: src/a.lua:5: see x.lua:9: y");
        assert_eq!(line, Some(5));
        assert_eq!(msg, "see x.lua:9: y");

        // A dashboard script that raises a message shaped like a location.
        let (line, msg) = parse_lua_line("[string \"script\"]:4: x.lua:3: y");
        assert_eq!(line, Some(4));
        assert_eq!(msg, "x.lua:3: y");

        let (line, _) = parse_lua_line("runtime error: oops t[1]:2: x");
        assert_eq!(line, None);
    }

    #[test]
    fn parse_lua_line_drops_the_traceback() {
        let (line, msg) = parse_lua_line(
            "runtime error: [string \"script\"]:4: boom\nstack traceback:\n\t[C]: in function 'error'\n\t[string \"script\"]:4: in function 'handle'",
        );
        assert_eq!(line, Some(4));
        assert_eq!(msg, "boom");
        let (line, msg) =
            parse_lua_line("runtime error: bare\nstack traceback:\n\t[C]: in function 'error'");
        assert_eq!(line, None);
        assert_eq!(msg, "runtime error: bare");
    }

    /// The hand-written strings above pin the parser's shape; this pins that a
    /// script run the way the runners run it produces that shape.
    #[cfg(feature = "lua-reference")]
    #[tokio::test]
    async fn parse_lua_line_reads_the_line_of_a_real_script_error() {
        use crate::lua::sandbox::{call_handle, create_sandbox, load_script};

        let lua = create_sandbox().unwrap();
        load_script(
            &lua,
            "function handle()\n  local t = nil\n  return t.x\nend",
        )
        .exec()
        .unwrap();
        let handle: mlua::Function = lua.globals().get("handle").unwrap();
        let raw = call_handle(&lua, &handle, ())
            .await
            .unwrap_err()
            .to_string();

        let (line, message) = parse_lua_line(&raw);
        assert_eq!(line, Some(3), "{raw}");
        assert_eq!(message, "attempt to index a nil value (local 't')");
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
        assert_eq!(
            serde_json::to_string(&ScriptErrorType::Memory).unwrap(),
            "\"memory\""
        );
        assert_eq!(ScriptErrorType::Memory.to_string(), "memory");
    }

    #[test]
    fn every_script_error_kind_has_exactly_one_script_error_type() {
        use crate::plugin::ScriptErrorKind;
        assert_eq!(ScriptErrorKind::ALL.len(), ScriptErrorType::ALL.len());
        for kind in ScriptErrorKind::ALL {
            let error_type = ScriptErrorType::from(kind);
            assert_eq!(error_type.to_string(), kind.as_str());
            assert_eq!(
                serde_json::to_value(error_type).unwrap(),
                serde_json::to_value(kind).unwrap()
            );
        }
        for error_type in ScriptErrorType::ALL {
            let kind: ScriptErrorKind = error_type
                .to_string()
                .parse()
                .unwrap_or_else(|_| panic!("{error_type} has no wire kind"));
            assert_eq!(
                ScriptErrorType::from(kind).to_string(),
                error_type.to_string()
            );
        }
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
                file: None,
            }
            .to_string(),
            "script runtime error in test.method at line 5: oops"
        );
    }
}
