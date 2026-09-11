//! Guest-side SDK for HappyView WASM plugins.
//!
//! A plugin describes itself and its exports; the SDK owns everything between
//! that and the host — the allocator, the packed-`i64` calling convention, the
//! JSON envelope, and the `env` host imports. Nothing in a plugin crate should
//! need `extern "C"`, a raw pointer, or a `#[global_allocator]`.
//!
//! ```ignore
//! use happyview_plugin_sdk::{
//!     library_plugin, ApiSurface, CallContext, PluginError, PluginInfo, Value,
//! };
//!
//! library_plugin! {
//!     info: PluginInfo::new("echo", "Echo", "1.0.0"),
//!     surface: surface,
//!     call: dispatch,
//! }
//!
//! fn surface() -> ApiSurface { ApiSurface::new("echo") }
//!
//! fn dispatch(function: &str, args: &[Value], _ctx: &CallContext)
//!     -> Result<Value, PluginError>
//! {
//!     match function {
//!         "echo" => Ok(args.first().cloned().unwrap_or(Value::Null)),
//!         other => Err(PluginError::unknown_function(other)),
//!     }
//! }
//! ```
//!
//! An auth plugin uses [`auth_plugin!`] instead, which emits the four exports
//! the external-auth flow calls — `get_authorize_url`, `handle_callback`,
//! `refresh_tokens` and `get_profile` — each with its own typed input and
//! output.
//!
//! Native builds compile the whole SDK, so a plugin's own logic is testable
//! with `cargo test`; the host wrappers report [`host::HostError::NotWasm`]
//! there rather than calling anything.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

pub mod abi;
pub mod envelope;
pub mod host;
mod macros;
pub mod types;

pub use envelope::{PluginError, Response};
pub use types::{
    ApiExport, ApiSurface, AuthorizeUrlInput, CallContext, CallInput, CallbackInput,
    ExternalProfile, PluginInfo, RefreshInput, StrongRef, TokenInput, TokenSet,
};

/// Items the macros name in their expansion. Not a public API: a plugin crate
/// compiles with `no_std`, so the macros cannot assume `String` is in scope
/// where they land.
#[doc(hidden)]
pub mod __private {
    pub use alloc::string::String;
}

pub use serde_json;
/// Re-exported so a plugin needs only this crate as a dependency.
pub use serde_json::{json, Map, Value};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_envelope_round_trips() {
        let envelope = Response::Ok {
            ok: json!({"status": 200}),
        };
        let text = serde_json::to_string(&envelope).unwrap();
        assert_eq!(text, r#"{"ok":{"status":200}}"#);
        let parsed: Response<Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.into_result().unwrap(), json!({"status": 200}));
    }

    #[test]
    fn error_envelope_round_trips() {
        let envelope: Response<Value> = Response::Err {
            error: PluginError::new("HTTP_ERROR", "boom").retryable(),
        };
        let text = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            text,
            r#"{"error":{"code":"HTTP_ERROR","message":"boom","retryable":true}}"#
        );
        let parsed: Response<Value> = serde_json::from_str(&text).unwrap();
        let err = parsed.into_result().unwrap_err();
        assert_eq!(err.code, "HTTP_ERROR");
        assert!(err.retryable);
    }

    #[test]
    fn retryable_defaults_to_false_when_the_host_omits_it() {
        let parsed: Response<Value> =
            serde_json::from_str(r#"{"error":{"code":"X","message":"y"}}"#).unwrap();
        assert!(!parsed.into_result().unwrap_err().retryable);
    }

    #[test]
    fn error_constructors_use_the_codes_the_host_expects() {
        assert_eq!(PluginError::bad_input("nope").code, "BAD_INPUT");
        assert_eq!(PluginError::host("nope").code, "HOST_ERROR");

        let unknown = PluginError::unknown_function("frobnicate");
        assert_eq!(unknown.code, "UNKNOWN_FUNCTION");
        assert_eq!(unknown.message, "no such function: frobnicate");
        assert!(!unknown.retryable);

        let parse_error = serde_json::from_str::<Value>("{oops").unwrap_err();
        assert_eq!(PluginError::from(parse_error).code, "BAD_INPUT");
    }

    #[test]
    fn an_ok_envelope_holding_an_error_key_still_parses_as_ok() {
        // `Response` is untagged, so ordering matters: `{"ok": ...}` must win.
        let parsed: Response<Value> = serde_json::from_str(r#"{"ok":{"error":"inner"}}"#).unwrap();
        assert_eq!(parsed.into_result().unwrap(), json!({"error": "inner"}));
    }
}
