//! Pins the object model and the record/table host imports: `chain` echoes
//! the document an immediate method receives, and each `*_query` function is
//! a bare pass-through to one host import.

#![cfg_attr(target_arch = "wasm32", no_std)]

use happyview_plugin_sdk::host;
use happyview_plugin_sdk::{
    library_plugin, ApiExport, ApiSurface, BacklinksQuery, CallContext, ObjectCall, PluginError,
    PluginInfo, RecordsCount, RecordsQuery, RecordsSearch, TableQuery, Value,
};

library_plugin! {
    info: PluginInfo::new("sdk_objects", "Objects fixture", "1.0.0"),
    surface: surface,
    call: dispatch,
}

fn surface() -> ApiSurface {
    ApiSurface::new("objects")
        .export(ApiExport::constructor("chain").lazy("add").immediate("doc"))
        .exports(
            [
                "records_query",
                "records_count",
                "records_get",
                "records_search",
                "table_query",
                "backlinks_query",
                "backend",
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

fn dispatch(function: &str, args: &[Value], ctx: &CallContext) -> Result<Value, PluginError> {
    match function {
        "backend" => Ok(ctx
            .db_backend
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null)),
        "chain" => {
            let call = ObjectCall::from_args(args)?;
            if call.call.name != "doc" {
                return Err(PluginError::new("BAD_CHAIN", "unknown method"));
            }
            serde_json::to_value(&call).map_err(PluginError::from)
        }
        "records_query" => serde_json::to_value(host::records_query(&one::<RecordsQuery>(args)?)?)
            .map_err(PluginError::from),
        "records_count" => Ok(Value::from(host::records_count(&one::<RecordsCount>(
            args,
        )?)?)),
        "records_get" => {
            let uri = args
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| PluginError::bad_input("uri is required"))?;
            Ok(host::records_get(uri)?.unwrap_or(Value::Null))
        }
        "records_search" => Ok(Value::Array(host::records_search(&one::<RecordsSearch>(
            args,
        )?)?)),
        "table_query" => host::table_query(&one::<TableQuery>(args)?),
        "backlinks_query" => {
            serde_json::to_value(host::backlinks_query(&one::<BacklinksQuery>(args)?)?)
                .map_err(PluginError::from)
        }
        other => Err(PluginError::unknown_function(other)),
    }
}
