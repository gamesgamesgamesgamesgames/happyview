//! `http` standard library plugin: outbound HTTP requests through the host.
//! Exports `get`, `post`, `put`, `patch`, `delete` and `head`.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use happyview_plugin_sdk::host::{self, HttpRequest};
use happyview_plugin_sdk::{
    json, library_plugin, ApiExport, ApiSurface, CallContext, Map, PluginError, PluginInfo, Value,
};

const METHODS: [&str; 6] = ["get", "post", "put", "patch", "delete", "head"];

library_plugin! {
    info: PluginInfo::new("sdk_http", "HTTP Client", "1.0.0"),
    surface: surface,
    call: dispatch,
}

fn surface() -> ApiSurface {
    let opts = json!({"name": "opts", "type": "object?", "description": "Request options", "properties": [
        {"name": "headers", "type": "object?", "description": "Header name to value"},
        {"name": "body", "type": "string?", "description": "Request body (ignored for get/head)"}
    ]});
    let returns = json!({"type": "object", "properties": [
        {"name": "status", "type": "integer"},
        {"name": "body", "type": "string"},
        {"name": "headers", "type": "object", "description": "Lower-cased header name to value"}
    ]});
    ApiSurface::new("http")
        .describe("Outbound HTTP requests")
        .exports(METHODS.iter().map(|method| {
            ApiExport::function(*method)
                .describe(format!("Send a {} request", method.to_uppercase()))
                .param("url", "string", "Target URL")
                .param_json(opts.clone())
                .returns(returns.clone())
        }))
}

fn dispatch(function: &str, args: &[Value], _ctx: &CallContext) -> Result<Value, PluginError> {
    if !METHODS.contains(&function) {
        return Err(PluginError::unknown_function(function));
    }
    request(function, args)
}

fn request(method: &str, args: &[Value]) -> Result<Value, PluginError> {
    let url = args
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::bad_input("url is required"))?;
    let opts = args.get(1).unwrap_or(&Value::Null);

    // A body is meaningless on get/head, so it is dropped rather than sent.
    let body = match (method, &opts["body"]) {
        ("get" | "head", _) | (_, Value::Null) => None,
        (_, Value::String(text)) => Some(text.clone().into_bytes()),
        (_, value) => Some(value.to_string().into_bytes()),
    };
    let response = host::http_request(&HttpRequest {
        method: method.to_uppercase(),
        url: url.to_string(),
        headers: request_headers(opts),
        body,
    })?;

    // Header names arrive however the server wrote them; scripts index this map
    // by name, so lower-case it once here.
    let mut headers = Map::new();
    for (name, value) in &response.headers {
        headers.insert(name.to_lowercase(), Value::String(value.clone()));
    }
    // A HEAD response has no body to report even if the server sent one.
    let body = if method == "head" {
        String::new()
    } else {
        response.text().into_owned()
    };
    Ok(json!({"status": response.status, "body": body, "headers": headers}))
}

fn request_headers(opts: &Value) -> Vec<(String, String)> {
    opts["headers"]
        .as_object()
        .map(|headers| {
            headers
                .iter()
                .map(|(name, value)| {
                    let value = match value {
                        Value::String(text) => text.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    (name.clone(), value)
                })
                .collect()
        })
        .unwrap_or_default()
}
