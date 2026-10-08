//! Pins the three blob host imports: one pass-through function per SDK
//! wrapper, plus `exists`, which the SDK derives from `stat` rather than
//! importing — calling it here is what says that derivation still works.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::vec::Vec;

use happyview_plugin_sdk::host;
use happyview_plugin_sdk::{
    library_plugin, ApiExport, ApiSurface, CallContext, PluginError, PluginInfo, Value,
};

library_plugin! {
    info: PluginInfo::new("sdk_blobs", "Blobs fixture", "1.0.0"),
    surface: surface,
    call: dispatch,
}

fn surface() -> ApiSurface {
    ApiSurface::new("blobs_fixture").exports(
        ["put", "get", "stat", "exists"]
            .into_iter()
            .map(ApiExport::function),
    )
}

fn cid_arg(args: &[Value]) -> Result<&str, PluginError> {
    args.first()
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::bad_input("cid is required"))
}

fn dispatch(function: &str, args: &[Value], _ctx: &CallContext) -> Result<Value, PluginError> {
    match function {
        // Bytes come in as an array of integers so a test can send content
        // that is not valid UTF-8, which is the case worth pinning: a string
        // argument could only ever exercise the other branch of the wire's
        // body encoding.
        "put" => {
            let [bytes, mime] = args else {
                return Err(PluginError::bad_input("expected bytes and a mime type"));
            };
            let bytes: Vec<u8> = serde_json::from_value(bytes.clone())?;
            let mime = mime
                .as_str()
                .ok_or_else(|| PluginError::bad_input("mime must be a string"))?;
            Ok(Value::from(host::blob_put(&bytes, mime)?))
        }
        "get" => match host::blob_get(cid_arg(args)?)? {
            Some(blob) => serde_json::to_value(blob).map_err(PluginError::from),
            None => Ok(Value::Null),
        },
        "stat" => match host::blob_stat(cid_arg(args)?)? {
            Some(info) => serde_json::to_value(info).map_err(PluginError::from),
            None => Ok(Value::Null),
        },
        "exists" => Ok(Value::from(host::blob_exists(cid_arg(args)?)?)),
        other => Err(PluginError::unknown_function(other)),
    }
}
