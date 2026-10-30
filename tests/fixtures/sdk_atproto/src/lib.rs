//! Pins the five atproto/attestation host imports: one pass-through function
//! per SDK wrapper. `resolve_service` takes a bare DID string, the other four
//! take a single spec argument.

#![cfg_attr(target_arch = "wasm32", no_std)]

use happyview_plugin_sdk::host;
use happyview_plugin_sdk::{
    library_plugin, ApiExport, ApiSurface, AtprotoBlobDownload, AttestSign, AttestVerify,
    CallContext, LabelsGet, PluginError, PluginInfo, Value,
};

library_plugin! {
    info: PluginInfo::new("sdk_atproto", "Atproto fixture", "1.0.0"),
    surface: surface,
    call: dispatch,
}

fn surface() -> ApiSurface {
    ApiSurface::new("atproto_fixture").exports(
        [
            "resolve_service",
            "blob_download",
            "labels_get",
            "attest_sign",
            "attest_verify",
        ]
        .into_iter()
        .map(ApiExport::function),
    )
}

fn one<T: serde::de::DeserializeOwned>(args: &[Value]) -> Result<T, PluginError> {
    let [spec] = args else {
        return Err(PluginError::bad_input("expected one argument"));
    };
    serde_json::from_value(spec.clone()).map_err(PluginError::from)
}

fn dispatch(function: &str, args: &[Value], _ctx: &CallContext) -> Result<Value, PluginError> {
    match function {
        "resolve_service" => {
            let did = args
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| PluginError::bad_input("did is required"))?;
            Ok(host::atproto_resolve_service(did)?
                .map(Value::from)
                .unwrap_or(Value::Null))
        }
        "blob_download" => serde_json::to_value(host::atproto_blob_download(&one::<
            AtprotoBlobDownload,
        >(args)?)?)
        .map_err(PluginError::from),
        "labels_get" => serde_json::to_value(host::labels_get(&one::<LabelsGet>(args)?)?)
            .map_err(PluginError::from),
        "attest_sign" => host::attest_sign(&one::<AttestSign>(args)?),
        "attest_verify" => Ok(Value::from(host::attest_verify(&one::<AttestVerify>(
            args,
        )?)?)),
        other => Err(PluginError::unknown_function(other)),
    }
}
