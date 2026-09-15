//! Pins the seven linked-repo/jobs host imports: one pass-through function
//! per SDK wrapper. `list` takes no argument; every other function takes a
//! single spec argument.

#![cfg_attr(target_arch = "wasm32", no_std)]

use happyview_plugin_sdk::host;
use happyview_plugin_sdk::{
    library_plugin, ApiExport, ApiSurface, CallContext, JobCreate, LinkedRepoBlobUpload,
    LinkedRepoCall, LinkedRepoRecordCreate, LinkedRepoRecordDelete, LinkedRepoRecordPut,
    PluginError, PluginInfo, Value,
};

library_plugin! {
    info: PluginInfo::new("sdk_linked_repos", "Linked repos fixture", "1.0.0"),
    surface: surface,
    call: dispatch,
}

fn surface() -> ApiSurface {
    ApiSurface::new("linked_fixture").exports(
        [
            "list",
            "create_record",
            "put_record",
            "delete_record",
            "upload_blob",
            "call",
            "jobs_create",
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
        "list" => serde_json::to_value(host::linked_repos_list()?).map_err(PluginError::from),
        "create_record" => serde_json::to_value(host::linked_repo_create_record(&one::<
            LinkedRepoRecordCreate,
        >(args)?)?)
        .map_err(PluginError::from),
        "put_record" => serde_json::to_value(host::linked_repo_put_record(&one::<
            LinkedRepoRecordPut,
        >(args)?)?)
        .map_err(PluginError::from),
        "delete_record" => {
            host::linked_repo_delete_record(&one::<LinkedRepoRecordDelete>(args)?)?;
            Ok(Value::Null)
        }
        "upload_blob" => host::linked_repo_upload_blob(&one::<LinkedRepoBlobUpload>(args)?),
        "call" => host::linked_repo_call(&one::<LinkedRepoCall>(args)?),
        "jobs_create" => Ok(Value::from(host::jobs_create(&one::<JobCreate>(args)?)?)),
        other => Err(PluginError::unknown_function(other)),
    }
}
