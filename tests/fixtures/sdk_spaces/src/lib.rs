//! Pins the fifteen spaces host imports: one pass-through function per SDK
//! wrapper, named like the wrapper without the `spaces_` prefix. Every
//! function takes a single spec argument.

#![cfg_attr(target_arch = "wasm32", no_std)]

use happyview_plugin_sdk::host;
use happyview_plugin_sdk::{
    ApiExport, ApiSurface, CallContext, PluginError, PluginInfo, SpaceDelete, SpaceInviteCreate,
    SpaceMemberAdd, SpaceMemberRemove, SpaceRecordDelete, SpaceRecordPut, SpaceRecordWrite,
    SpaceUpdate, SpacesAcceptInvite, SpacesAccess, SpacesCreate, SpacesInfo, SpacesMembers,
    SpacesQuery, Value, library_plugin,
};

library_plugin! {
    info: PluginInfo::new("sdk_spaces", "Spaces fixture", "1.0.0"),
    surface: surface,
    call: dispatch,
}

fn surface() -> ApiSurface {
    ApiSurface::new("spaces_fixture").exports(
        [
            "info",
            "query",
            "members",
            "access",
            "create",
            "accept_invite",
            "write_record",
            "put_record",
            "delete_record",
            "add_member",
            "set_member",
            "remove_member",
            "update",
            "delete",
            "create_invite",
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
        "info" => serde_json::to_value(host::spaces_info(&one::<SpacesInfo>(args)?)?)
            .map_err(PluginError::from),
        "query" => serde_json::to_value(host::spaces_query(&one::<SpacesQuery>(args)?)?)
            .map_err(PluginError::from),
        "members" => serde_json::to_value(host::spaces_members(&one::<SpacesMembers>(args)?)?)
            .map_err(PluginError::from),
        "access" => serde_json::to_value(host::spaces_access(&one::<SpacesAccess>(args)?)?)
            .map_err(PluginError::from),
        "create" => serde_json::to_value(host::spaces_create(&one::<SpacesCreate>(args)?)?)
            .map_err(PluginError::from),
        "accept_invite" => serde_json::to_value(host::spaces_accept_invite(&one::<
            SpacesAcceptInvite,
        >(args)?)?)
        .map_err(PluginError::from),
        "write_record" => {
            serde_json::to_value(host::spaces_write_record(&one::<SpaceRecordWrite>(args)?)?)
                .map_err(PluginError::from)
        }
        "put_record" => {
            serde_json::to_value(host::spaces_put_record(&one::<SpaceRecordPut>(args)?)?)
                .map_err(PluginError::from)
        }
        "delete_record" => {
            host::spaces_delete_record(&one::<SpaceRecordDelete>(args)?)?;
            Ok(Value::Null)
        }
        "add_member" => {
            serde_json::to_value(host::spaces_add_member(&one::<SpaceMemberAdd>(args)?)?)
                .map_err(PluginError::from)
        }
        "set_member" => {
            serde_json::to_value(host::spaces_set_member(&one::<SpaceMemberAdd>(args)?)?)
                .map_err(PluginError::from)
        }
        "remove_member" => {
            host::spaces_remove_member(&one::<SpaceMemberRemove>(args)?)?;
            Ok(Value::Null)
        }
        "update" => serde_json::to_value(host::spaces_update(&one::<SpaceUpdate>(args)?)?)
            .map_err(PluginError::from),
        "delete" => {
            host::spaces_delete(&one::<SpaceDelete>(args)?)?;
            Ok(Value::Null)
        }
        "create_invite" => serde_json::to_value(host::spaces_create_invite(&one::<
            SpaceInviteCreate,
        >(args)?)?)
        .map_err(PluginError::from),
        other => Err(PluginError::unknown_function(other)),
    }
}
