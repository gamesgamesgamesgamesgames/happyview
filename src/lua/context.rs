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

impl From<&SpaceContext> for crate::plugin::ScriptSpace {
    fn from(space: &SpaceContext) -> Self {
        Self {
            uri: space.space.clone(),
            id: space.space_id.clone(),
            did: space.did.clone(),
            authority_did: space.authority_did.clone(),
            type_nsid: space.type_nsid.clone(),
            skey: space.skey.clone(),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::sandbox::create_sandbox;
    use serde_json::json;

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

    #[tokio::test]
    async fn ctx_carries_a_delegate_and_the_space_fields() {
        let lua = create_sandbox().unwrap();
        let env = HashMap::new();
        let space = SpaceContext {
            space: "at://did:plc:owner/space/com.example.forum/main".into(),
            space_id: "space-123".into(),
            did: "did:plc:owner".into(),
            authority_did: "did:plc:owner".into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
        };
        let ctx = build_ctx(
            &lua,
            &Invocation {
                trigger_id: "xrpc.procedure:com.example.post",
                caller_did: Some("did:plc:caller"),
                has_pds_auth: true,
                env: &env,
                method: Some("com.example.post"),
                collection: Some("com.example.forum.post"),
                params: None,
                delegate_did: Some("did:plc:delegate"),
                space: Some(&space),
                job: None,
            },
        )
        .unwrap();
        lua.globals().set("ctx", ctx).unwrap();
        let s: String = lua
            .load(
                r#"return ctx.delegate_did .. "|" .. ctx.space.uri .. "|" .. ctx.space.id .. "|" .. ctx.space.did .. "|" .. ctx.space.type_nsid .. "|" .. ctx.space.skey"#,
            )
            .eval()
            .unwrap();
        assert_eq!(
            s,
            "did:plc:delegate|at://did:plc:owner/space/com.example.forum/main|space-123|did:plc:owner|com.example.forum|main"
        );
    }

    #[tokio::test]
    async fn ctx_job_carries_only_the_canonical_keys() {
        let lua = create_sandbox().unwrap();
        let env = HashMap::new();
        let job = lua.create_table().unwrap();
        job.set("id", "job-1").unwrap();
        job.set("progress", lua.create_function(|_, ()| Ok(())).unwrap())
            .unwrap();
        job.set(
            "should_stop",
            lua.create_function(|_, ()| Ok(false)).unwrap(),
        )
        .unwrap();
        job.set("wait", lua.create_function(|_, _s: f64| Ok(())).unwrap())
            .unwrap();
        job.set("extra", "dropped").unwrap();
        let ctx = build_ctx(
            &lua,
            &Invocation {
                trigger_id: "job.run:test.export",
                caller_did: Some("did:plc:me"),
                has_pds_auth: false,
                env: &env,
                method: None,
                collection: None,
                params: None,
                delegate_did: None,
                space: None,
                job: Some(job),
            },
        )
        .unwrap();
        lua.globals().set("ctx", ctx).unwrap();
        let s: String = lua
            .load(
                r#"return ctx.job.id .. "|" .. type(ctx.job.progress) .. "|" .. tostring(ctx.job.should_stop()) .. "|" .. type(ctx.job.wait) .. "|" .. tostring(ctx.job.extra)"#,
            )
            .eval()
            .unwrap();
        assert_eq!(s, "job-1|function|false|function|nil");
    }
}
