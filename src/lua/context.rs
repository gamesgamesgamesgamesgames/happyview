use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
use serde_json::Value;
use std::collections::HashMap;

/// Optional space context passed to Lua scripts when the request is space-scoped.
#[derive(Debug, Clone)]
pub struct SpaceContext {
    pub space: String,
    pub space_id: String,
    pub did: String,
    pub authority_did: String,
    pub type_nsid: String,
    pub skey: String,
}

fn set_space_context(lua: &Lua, space: Option<&SpaceContext>) -> LuaResult<()> {
    let globals = lua.globals();
    match space {
        Some(ctx) => {
            let table = lua.create_table()?;
            table.set("space", ctx.space.as_str())?;
            table.set("space_id", ctx.space_id.as_str())?;
            table.set("did", ctx.did.as_str())?;
            table.set("authority_did", ctx.authority_did.as_str())?;
            table.set("type_nsid", ctx.type_nsid.as_str())?;
            table.set("skey", ctx.skey.as_str())?;
            globals.set("space", table)?;
        }
        None => {
            globals.set("space", mlua::Value::Nil)?;
        }
    }
    Ok(())
}

/// Set global context variables for a procedure script.
#[allow(clippy::too_many_arguments)]
pub fn set_procedure_context(
    lua: &Lua,
    method: &str,
    input: &Value,
    params: &HashMap<String, Value>,
    caller_did: &str,
    collection: &str,
    space: Option<&SpaceContext>,
    delegate_did: Option<&str>,
) -> LuaResult<()> {
    let globals = lua.globals();
    globals.set("method", method.to_string())?;
    globals.set("input", lua.to_value(input)?)?;
    globals.set("params", lua.to_value(params)?)?;
    globals.set("caller_did", caller_did.to_string())?;
    globals.set("collection", collection.to_string())?;
    match delegate_did {
        Some(did) => globals.set("delegate_did", did.to_string())?,
        None => globals.set("delegate_did", mlua::Value::Nil)?,
    }
    set_space_context(lua, space)?;
    Ok(())
}

/// Set global context variables for a query script.
pub fn set_query_context(
    lua: &Lua,
    method: &str,
    params: &HashMap<String, Value>,
    collection: &str,
    caller_did: Option<&str>,
    space: Option<&SpaceContext>,
) -> LuaResult<()> {
    let globals = lua.globals();
    globals.set("method", method.to_string())?;
    globals.set("params", lua.to_value(params)?)?;
    globals.set("collection", collection.to_string())?;
    match caller_did {
        Some(did) => globals.set("caller_did", did.to_string())?,
        None => globals.set("caller_did", mlua::Value::Nil)?,
    }
    set_space_context(lua, space)?;
    Ok(())
}

/// Set the `env` global table from script variables.
pub fn set_env_context(lua: &Lua, vars: &HashMap<String, String>) -> LuaResult<()> {
    let globals = lua.globals();
    globals.set("env", lua.to_value(vars)?)?;
    Ok(())
}

/// Everything the runtime knows about one script invocation, built into the
/// `ctx` argument of `handle(input, ctx)`. Keys are snake_case, the same
/// convention as everything else a Lua script sees.
pub struct Invocation<'a> {
    pub trigger_id: &'a str,
    pub caller_did: Option<&'a str>,
    pub has_pds_auth: bool,
    pub env: &'a HashMap<String, String>,
    pub method: Option<&'a str>,
    pub collection: Option<&'a str>,
    pub params: Option<&'a HashMap<String, Value>>,
    pub delegate_did: Option<&'a str>,
    pub space: Option<&'a SpaceContext>,
    pub job: Option<mlua::Table>,
}

/// Build the `ctx` table passed as the second argument to `handle`.
pub fn build_ctx(lua: &Lua, inv: &Invocation<'_>) -> LuaResult<mlua::Table> {
    let ctx = lua.create_table()?;
    ctx.set("trigger", inv.trigger_id)?;
    ctx.set("caller_did", inv.caller_did.map(str::to_string))?;
    ctx.set("has_pds_auth", inv.has_pds_auth)?;
    ctx.set("env", lua.to_value(inv.env)?)?;
    ctx.set("method", inv.method.map(str::to_string))?;
    ctx.set("collection", inv.collection.map(str::to_string))?;
    ctx.set(
        "params",
        match inv.params {
            Some(p) => lua.to_value(p)?,
            None => mlua::Value::Nil,
        },
    )?;
    ctx.set("delegate_did", inv.delegate_did.map(str::to_string))?;
    if let Some(s) = inv.space {
        let t = lua.create_table()?;
        t.set("uri", s.space.as_str())?;
        t.set("id", s.space_id.as_str())?;
        t.set("did", s.did.as_str())?;
        t.set("authority_did", s.authority_did.as_str())?;
        t.set("type_nsid", s.type_nsid.as_str())?;
        t.set("skey", s.skey.as_str())?;
        ctx.set("space", t)?;
    }
    if let Some(job) = &inv.job {
        let t = lua.create_table()?;
        for canonical in ["id", "progress", "should_stop", "wait"] {
            t.set(canonical, job.get::<mlua::Value>(canonical)?)?;
        }
        ctx.set("job", t)?;
    }
    Ok(ctx)
}

/// Set global context variables for an index hook script.
pub fn set_hook_context(
    lua: &Lua,
    action: &str,
    uri: &str,
    did: &str,
    collection: &str,
    rkey: &str,
    record: Option<&Value>,
) -> LuaResult<()> {
    let globals = lua.globals();
    globals.set("action", action.to_string())?;
    globals.set("uri", uri.to_string())?;
    globals.set("did", did.to_string())?;
    globals.set("collection", collection.to_string())?;
    globals.set("rkey", rkey.to_string())?;
    match record {
        Some(r) => globals.set("record", lua.to_value(r)?)?,
        None => globals.set("record", mlua::Value::Nil)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::sandbox::create_sandbox;
    use serde_json::json;

    #[test]
    fn hook_context_sets_all_globals() {
        let lua = create_sandbox().unwrap();
        let record = json!({"name": "Test Game"});
        set_hook_context(
            &lua,
            "create",
            "at://did:plc:abc/col/rkey",
            "did:plc:abc",
            "col",
            "rkey",
            Some(&record),
        )
        .unwrap();

        let globals = lua.globals();
        assert_eq!(globals.get::<String>("action").unwrap(), "create");
        assert_eq!(
            globals.get::<String>("uri").unwrap(),
            "at://did:plc:abc/col/rkey"
        );
        assert_eq!(globals.get::<String>("did").unwrap(), "did:plc:abc");
        assert_eq!(globals.get::<String>("collection").unwrap(), "col");
        assert_eq!(globals.get::<String>("rkey").unwrap(), "rkey");

        let rec: mlua::Table = globals.get("record").unwrap();
        assert_eq!(rec.get::<String>("name").unwrap(), "Test Game");
    }

    #[test]
    fn procedure_context_sets_all_globals() {
        let lua = create_sandbox().unwrap();
        let input = json!({"key": "val"});
        let mut params = HashMap::new();
        params.insert("limit".to_string(), json!(10));
        set_procedure_context(
            &lua,
            "com.example.doThing",
            &input,
            &params,
            "did:plc:test",
            "com.example.thing",
            None,
            None,
        )
        .unwrap();

        let globals = lua.globals();
        assert_eq!(
            globals.get::<String>("method").unwrap(),
            "com.example.doThing"
        );
        assert_eq!(globals.get::<String>("caller_did").unwrap(), "did:plc:test");
        assert_eq!(
            globals.get::<String>("collection").unwrap(),
            "com.example.thing"
        );
        assert!(globals.get::<mlua::Value>("delegate_did").unwrap().is_nil());

        let input_table: mlua::Table = globals.get("input").unwrap();
        assert_eq!(input_table.get::<String>("key").unwrap(), "val");

        let params_table: mlua::Table = globals.get("params").unwrap();
        assert_eq!(params_table.get::<i64>("limit").unwrap(), 10);
    }

    #[test]
    fn query_context_sets_all_globals() {
        let lua = create_sandbox().unwrap();
        let mut params = HashMap::new();
        params.insert("limit".to_string(), json!("10"));
        params.insert("cursor".to_string(), json!("abc"));
        set_query_context(
            &lua,
            "com.example.listThings",
            &params,
            "com.example.thing",
            Some("did:plc:test"),
            None,
        )
        .unwrap();

        let globals = lua.globals();
        assert_eq!(
            globals.get::<String>("method").unwrap(),
            "com.example.listThings"
        );
        assert_eq!(
            globals.get::<String>("collection").unwrap(),
            "com.example.thing"
        );

        let params_table: mlua::Table = globals.get("params").unwrap();
        assert_eq!(params_table.get::<String>("limit").unwrap(), "10");
        assert_eq!(params_table.get::<String>("cursor").unwrap(), "abc");
    }

    #[test]
    fn procedure_context_with_delegate_did() {
        let lua = create_sandbox().unwrap();
        let input = json!({"key": "val"});
        let params = HashMap::new();
        set_procedure_context(
            &lua,
            "com.example.doThing",
            &input,
            &params,
            "did:plc:caller",
            "com.example.thing",
            None,
            Some("did:plc:delegate"),
        )
        .unwrap();

        let globals = lua.globals();
        assert_eq!(
            globals.get::<String>("delegate_did").unwrap(),
            "did:plc:delegate"
        );
        assert_eq!(
            globals.get::<String>("caller_did").unwrap(),
            "did:plc:caller"
        );
    }

    #[test]
    fn env_context_sets_table() {
        let lua = create_sandbox().unwrap();
        let mut vars = HashMap::new();
        vars.insert("API_KEY".to_string(), "secret123".to_string());
        vars.insert("OTHER".to_string(), "value".to_string());
        set_env_context(&lua, &vars).unwrap();

        let globals = lua.globals();
        let env: mlua::Table = globals.get("env").unwrap();
        assert_eq!(env.get::<String>("API_KEY").unwrap(), "secret123");
        assert_eq!(env.get::<String>("OTHER").unwrap(), "value");
    }

    #[test]
    fn env_context_empty_map() {
        let lua = create_sandbox().unwrap();
        let vars = HashMap::new();
        set_env_context(&lua, &vars).unwrap();

        let globals = lua.globals();
        let env: mlua::Table = globals.get("env").unwrap();
        assert!(env.get::<mlua::Value>("anything").unwrap().is_nil());
    }

    #[test]
    fn query_context_with_space() {
        let lua = create_sandbox().unwrap();
        let params = HashMap::new();
        let space = SpaceContext {
            space: "at://did:plc:owner/space/com.example.forum/main".into(),
            space_id: "space-123".into(),
            did: "did:plc:owner".into(),
            authority_did: "did:plc:owner".into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
        };
        set_query_context(
            &lua,
            "com.example.listPosts",
            &params,
            "com.example.forum.post",
            Some("did:plc:test"),
            Some(&space),
        )
        .unwrap();

        let globals = lua.globals();
        let space_table: mlua::Table = globals.get("space").unwrap();
        assert_eq!(
            space_table.get::<String>("space").unwrap(),
            "at://did:plc:owner/space/com.example.forum/main"
        );
        assert_eq!(space_table.get::<String>("space_id").unwrap(), "space-123");
        assert_eq!(space_table.get::<String>("did").unwrap(), "did:plc:owner");
    }

    #[test]
    fn query_context_without_space() {
        let lua = create_sandbox().unwrap();
        let params = HashMap::new();
        set_query_context(
            &lua,
            "com.example.listThings",
            &params,
            "com.example.thing",
            None,
            None,
        )
        .unwrap();

        let globals = lua.globals();
        assert!(globals.get::<mlua::Value>("space").unwrap().is_nil());
    }

    #[tokio::test]
    async fn ctx_carries_snake_case_keys_and_nils() {
        let lua = create_sandbox().unwrap();
        let env = HashMap::from([("API".to_string(), "k".to_string())]);
        let params = HashMap::from([("q".to_string(), json!("x"))]);
        let space = SpaceContext {
            space: "at://s".into(),
            space_id: "sid".into(),
            did: "did:plc:a".into(),
            authority_did: "did:plc:auth".into(),
            type_nsid: "t".into(),
            skey: "k".into(),
        };
        let ctx = build_ctx(
            &lua,
            &Invocation {
                trigger_id: "xrpc.procedure:app.test.p",
                caller_did: Some("did:plc:me"),
                has_pds_auth: true,
                env: &env,
                method: Some("app.test.p"),
                collection: Some("app.test.rec"),
                params: Some(&params),
                delegate_did: None,
                space: Some(&space),
                job: None,
            },
        )
        .unwrap();
        lua.globals().set("ctx", ctx).unwrap();
        let s: String = lua
            .load(
                r#"return ctx.caller_did .. "|" .. tostring(ctx.has_pds_auth) .. "|" .. ctx.env.API .. "|" .. ctx.trigger .. "|" .. ctx.method .. "|" .. ctx.collection .. "|" .. ctx.params.q .. "|" .. tostring(ctx.delegate_did) .. "|" .. ctx.space.authority_did .. "|" .. tostring(ctx.job)"#,
            )
            .eval()
            .unwrap();
        assert_eq!(
            s,
            "did:plc:me|true|k|xrpc.procedure:app.test.p|app.test.p|app.test.rec|x|nil|did:plc:auth|nil"
        );
    }

    #[tokio::test]
    async fn ctx_for_anonymous_query_has_nil_caller() {
        let lua = create_sandbox().unwrap();
        let env = HashMap::new();
        let ctx = build_ctx(
            &lua,
            &Invocation {
                trigger_id: "xrpc.query:app.test.q",
                caller_did: None,
                has_pds_auth: false,
                env: &env,
                method: Some("app.test.q"),
                collection: None,
                params: None,
                delegate_did: None,
                space: None,
                job: None,
            },
        )
        .unwrap();
        lua.globals().set("ctx", ctx).unwrap();
        let s: String = lua
            .load(
                r#"return tostring(ctx.caller_did) .. "|" .. tostring(ctx.space) .. "|" .. tostring(ctx.collection)"#,
            )
            .eval()
            .unwrap();
        assert_eq!(s, "nil|nil|nil");
    }

    #[test]
    fn hook_context_record_nil_on_delete() {
        let lua = create_sandbox().unwrap();
        set_hook_context(
            &lua,
            "delete",
            "at://did:plc:abc/col/rkey",
            "did:plc:abc",
            "col",
            "rkey",
            None,
        )
        .unwrap();

        let globals = lua.globals();
        assert_eq!(globals.get::<String>("action").unwrap(), "delete");
        assert!(globals.get::<mlua::Value>("record").unwrap().is_nil());
    }
}
