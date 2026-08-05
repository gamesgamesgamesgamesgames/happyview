use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
use std::sync::Arc;

use crate::AppState;
use crate::linked_repos::{db, pds, types};

pub(crate) struct LuaLinkedRepo {
    state: Arc<AppState>,
    grant_id: String,
    did: String,
    handle: Option<String>,
    status: String,
    scopes: String,
}

impl LuaLinkedRepo {
    /// Reload the grant so each call sees current status and scopes rather than
    /// whatever was true when the grant was created.
    async fn grant(&self) -> mlua::Result<types::LinkedRepo> {
        db::get(&self.state, &self.grant_id)
            .await
            .map_err(|e| mlua::Error::runtime(format!("{e}")))?
            .ok_or_else(|| mlua::Error::runtime(format!("linked repo {} is gone", self.did)))
    }
}

impl mlua::UserData for LuaLinkedRepo {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("did", |_, this| Ok(this.did.clone()));
        fields.add_field_method_get("handle", |_, this| Ok(this.handle.clone()));
        fields.add_field_method_get("status", |_, this| Ok(this.status.clone()));
        fields.add_field_method_get("scopes", |_, this| Ok(this.scopes.clone()));
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method("create_record", |lua, this, opts: mlua::Table| async move {
            let collection: String = opts
                .get("collection")
                .map_err(|_| mlua::Error::runtime("create_record: collection is required"))?;
            let record_val: mlua::Value = opts.get("record")?;
            if record_val.is_nil() {
                return Err(mlua::Error::runtime("create_record: record is required"));
            }
            let record: serde_json::Value = lua.from_value(record_val)?;
            let rkey: Option<String> = opts.get("rkey").ok();

            let grant = this.grant().await?;
            let (uri, cid) =
                pds::create_record(&this.state, &grant, &collection, rkey.as_deref(), record)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("create_record: {e}")))?;

            let result = lua.create_table()?;
            result.set("uri", uri)?;
            result.set("cid", cid)?;
            Ok(mlua::Value::Table(result))
        });

        methods.add_async_method("put_record", |lua, this, opts: mlua::Table| async move {
            let collection: String = opts
                .get("collection")
                .map_err(|_| mlua::Error::runtime("put_record: collection is required"))?;
            let rkey: String = opts
                .get("rkey")
                .map_err(|_| mlua::Error::runtime("put_record: rkey is required"))?;
            let record_val: mlua::Value = opts.get("record")?;
            if record_val.is_nil() {
                return Err(mlua::Error::runtime("put_record: record is required"));
            }
            let record: serde_json::Value = lua.from_value(record_val)?;
            let swap_cid: Option<String> = opts.get("swap_cid").ok();

            let grant = this.grant().await?;
            let (uri, cid) = pds::put_record(
                &this.state,
                &grant,
                &collection,
                &rkey,
                record,
                swap_cid.as_deref(),
            )
            .await
            .map_err(|e| mlua::Error::runtime(format!("put_record: {e}")))?;

            let result = lua.create_table()?;
            result.set("uri", uri)?;
            result.set("cid", cid)?;
            Ok(mlua::Value::Table(result))
        });

        methods.add_async_method(
            "delete_record",
            |_lua, this, opts: mlua::Table| async move {
                let collection: String = opts
                    .get("collection")
                    .map_err(|_| mlua::Error::runtime("delete_record: collection is required"))?;
                let rkey: String = opts
                    .get("rkey")
                    .map_err(|_| mlua::Error::runtime("delete_record: rkey is required"))?;

                let grant = this.grant().await?;
                pds::delete_record(&this.state, &grant, &collection, &rkey)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("delete_record: {e}")))?;
                Ok(true)
            },
        );

        methods.add_async_method(
            "upload_blob",
            |lua, this, (bytes, mime): (mlua::LuaString, String)| async move {
                let grant = this.grant().await?;
                let data = bytes.as_bytes().to_vec();
                let blob = pds::upload_blob(&this.state, &grant, &mime, data)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("upload_blob: {e}")))?;
                lua.to_value(&blob)
            },
        );

        methods.add_async_method(
            "call",
            |lua, this, (nsid, opts): (String, Option<mlua::Table>)| async move {
                let (params, input) = match opts {
                    Some(t) => {
                        let params_val: mlua::Value = t.get("params")?;
                        let input_val: mlua::Value = t.get("input")?;
                        let params = if params_val.is_nil() {
                            None
                        } else {
                            Some(lua.from_value::<serde_json::Value>(params_val)?)
                        };
                        let input = if input_val.is_nil() {
                            None
                        } else {
                            Some(lua.from_value::<serde_json::Value>(input_val)?)
                        };
                        (params, input)
                    }
                    None => (None, None),
                };

                let grant = this.grant().await?;
                let out = pds::call(&this.state, &grant, &nsid, params, input)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("call {nsid}: {e}")))?;
                lua.to_value(&out)
            },
        );
    }
}

pub fn register_linked_repos_api(lua: &Lua, state: Arc<AppState>) -> LuaResult<()> {
    let table = lua.create_table()?;

    let state_clone = state.clone();
    let list_fn = lua.create_async_function(move |lua, ()| {
        let state = state_clone.clone();
        async move {
            let grants = db::list(&state)
                .await
                .map_err(|e| mlua::Error::runtime(format!("linked_repos.list: {e}")))?;
            let out = lua.create_table()?;
            for (i, grant) in grants.iter().enumerate() {
                let entry = lua.create_table()?;
                entry.set("id", grant.id.clone())?;
                entry.set("did", grant.did.clone())?;
                entry.set("handle", grant.handle.clone())?;
                entry.set("reason", grant.reason.clone())?;
                entry.set("status", grant.status.clone())?;
                entry.set("scopes", grant.scopes.clone())?;
                out.set(i + 1, entry)?;
            }
            Ok(mlua::Value::Table(out))
        }
    })?;
    table.set("list", list_fn)?;

    let state_clone = state.clone();
    let get_fn = lua.create_async_function(move |lua, did: String| {
        let state = state_clone.clone();
        async move {
            let grant = db::get_by_did(&state, &did)
                .await
                .map_err(|e| mlua::Error::runtime(format!("linked_repos.get: {e}")))?;

            let Some(grant) = grant else {
                return Ok(mlua::Value::Nil);
            };

            if grant.status == types::STATUS_NEEDS_REAUTH {
                return Err(mlua::Error::runtime(format!(
                    "linked repo {did} needs reauthorization: {}",
                    grant.last_error.as_deref().unwrap_or("unknown error")
                )));
            }

            if grant.status == types::STATUS_PENDING {
                return Err(mlua::Error::runtime(format!(
                    "linked repo {did} has not been authorized yet"
                )));
            }

            let handle = LuaLinkedRepo {
                state: state.clone(),
                grant_id: grant.id.clone(),
                did: grant.did.clone().unwrap_or(did),
                handle: grant.handle.clone(),
                status: grant.status.clone(),
                scopes: grant.scopes.clone(),
            };
            Ok(mlua::Value::UserData(lua.create_userdata(handle)?))
        }
    })?;
    table.set("get", get_fn)?;

    lua.globals().set("linked_repos", table)?;
    Ok(())
}
