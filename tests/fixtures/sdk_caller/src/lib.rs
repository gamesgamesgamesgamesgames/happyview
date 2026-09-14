//! Pins the nine caller-acting and local-index host imports: one pass-through
//! function per SDK wrapper, each taking a single spec argument.

#![cfg_attr(target_arch = "wasm32", no_std)]

use happyview_plugin_sdk::host;
use happyview_plugin_sdk::{
    library_plugin, ApiExport, ApiSurface, CallContext, CallerBlobUpload, CallerRecordCreate,
    CallerRecordDelete, CallerRecordPut, CallerXrpcProcedure, CallerXrpcQuery, IndexDelete,
    IndexPut, PluginError, PluginInfo, Value,
};

library_plugin! {
    info: PluginInfo::new("sdk_caller", "Caller fixture", "1.0.0"),
    surface: surface,
    call: dispatch,
}

fn surface() -> ApiSurface {
    ApiSurface::new("caller").exports(
        [
            "create_record",
            "put_record",
            "delete_record",
            "upload_blob",
            "xrpc_query",
            "xrpc_procedure",
            "index_put",
            "index_delete",
            "lexicon_get",
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
        "create_record" => serde_json::to_value(host::caller_create_record(&one::<
            CallerRecordCreate,
        >(args)?)?)
        .map_err(PluginError::from),
        "put_record" => {
            serde_json::to_value(host::caller_put_record(&one::<CallerRecordPut>(args)?)?)
                .map_err(PluginError::from)
        }
        "delete_record" => {
            host::caller_delete_record(&one::<CallerRecordDelete>(args)?)?;
            Ok(Value::Null)
        }
        "upload_blob" => host::caller_upload_blob(&one::<CallerBlobUpload>(args)?),
        "xrpc_query" => host::caller_xrpc_query(&one::<CallerXrpcQuery>(args)?),
        "xrpc_procedure" => host::caller_xrpc_procedure(&one::<CallerXrpcProcedure>(args)?),
        "index_put" => serde_json::to_value(host::records_index_put(&one::<IndexPut>(args)?)?)
            .map_err(PluginError::from),
        "index_delete" => Ok(Value::from(host::records_index_delete(
            &one::<IndexDelete>(args)?,
        )?)),
        "lexicon_get" => {
            let nsid = args
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| PluginError::bad_input("nsid is required"))?;
            Ok(host::lexicon_get(nsid)?.unwrap_or(Value::Null))
        }
        other => Err(PluginError::unknown_function(other)),
    }
}
