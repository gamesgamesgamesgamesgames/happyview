#[cfg(feature = "lua-reference")]
use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
#[cfg(feature = "lua-reference")]
use serde_json::Value;
#[cfg(feature = "lua-reference")]
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
#[cfg(feature = "lua-reference")]
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
#[cfg(feature = "lua-reference")]
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

/// The `ctx.job` table a job script receives from a VM in this process: the
/// job's id, progress reporting, cooperative cancellation and sleep.
///
/// An interpreter reaches the same three controls through its `script:host`
/// imports, which act on the run the host is holding rather than on a job
/// named here, so this builds the surface only where there is no such run.
#[cfg(feature = "lua-reference")]
pub fn job_ctx(
    lua: &Lua,
    state: std::sync::Arc<crate::AppState>,
    job_id: String,
) -> LuaResult<mlua::Table> {
    let job_table = lua.create_table()?;
    job_table.set("id", job_id.clone())?;

    {
        let state = state.clone();
        let job_id = job_id.clone();
        let progress_fn = lua.create_async_function(move |lua, data: mlua::Value| {
            let state = state.clone();
            let job_id = job_id.clone();
            let json_data: serde_json::Value =
                lua.from_value(data).unwrap_or(serde_json::json!({}));
            async move {
                crate::jobs::db::update_progress(&state, &job_id, &json_data)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("job.progress failed: {e}")))?;
                Ok(())
            }
        })?;
        job_table.set("progress", progress_fn)?;
    }

    {
        let state = state.clone();
        let job_id = job_id.clone();
        let should_stop_fn = lua.create_async_function(move |_lua, ()| {
            let state = state.clone();
            let job_id = job_id.clone();
            async move {
                let result = crate::jobs::db::should_stop(&state, &job_id).await;
                Ok(result.is_some())
            }
        })?;
        job_table.set("should_stop", should_stop_fn)?;
    }

    {
        let wait_fn = lua.create_async_function(move |_lua, seconds: f64| {
            let state = state.clone();
            async move {
                let duration = std::time::Duration::from_secs_f64(seconds.clamp(0.0, 3600.0));
                tokio::time::sleep(duration).await;
                let elapsed_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
                crate::telemetry::counters::add_saturating(
                    &state.telemetry_counters.job_wait_ms,
                    elapsed_ms,
                );
                Ok(())
            }
        })?;
        job_table.set("wait", wait_fn)?;
    }

    Ok(job_table)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Six same-typed strings, where `space`/`space_id` and `uri`/`id` are the
    /// pair that do not share a name: a swap would compile and surface only as
    /// a space a script cannot find.
    #[test]
    fn a_space_context_maps_field_for_field_onto_the_wire_shape() {
        let sent = crate::plugin::ScriptSpace::from(&SpaceContext {
            space: "at://did:plc:a/space/com.example.type/sk".into(),
            space_id: "sid".into(),
            did: "did:plc:a".into(),
            authority_did: "did:plc:auth".into(),
            type_nsid: "com.example.type".into(),
            skey: "sk".into(),
        });
        assert_eq!(
            sent,
            crate::plugin::ScriptSpace {
                uri: "at://did:plc:a/space/com.example.type/sk".into(),
                id: "sid".into(),
                did: "did:plc:a".into(),
                authority_did: "did:plc:auth".into(),
                type_nsid: "com.example.type".into(),
                skey: "sk".into(),
            }
        );
    }
}

#[cfg(all(test, feature = "lua-reference"))]
mod reference_tests {
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
    async fn job_ctx_carries_the_id_and_the_three_functions() {
        let lua = create_sandbox().unwrap();
        let state =
            crate::test_support::test_state_with_pool(crate::test_support::memory_pool().await);
        let job = job_ctx(&lua, std::sync::Arc::new(state), "test-id".into()).unwrap();
        lua.globals().set("job", job).unwrap();

        let ok: bool = lua
            .load(
                r#"
                return job.id == 'test-id'
                    and type(job.progress) == 'function'
                    and type(job.should_stop) == 'function'
                    and type(job.wait) == 'function'
                "#,
            )
            .eval_async()
            .await
            .unwrap();
        assert!(ok);
    }

    #[tokio::test]
    async fn job_wait_clamps_to_the_allowed_range_and_counts_the_time() {
        let lua = create_sandbox().unwrap();
        let state =
            crate::test_support::test_state_with_pool(crate::test_support::memory_pool().await);
        let counters = state.telemetry_counters.clone();
        let job = job_ctx(&lua, std::sync::Arc::new(state), "test-id".into()).unwrap();
        lua.globals().set("job", job).unwrap();

        lua.load("job.wait(-5)").exec_async().await.unwrap();
        assert_eq!(
            counters
                .job_wait_ms
                .load(std::sync::atomic::Ordering::Relaxed),
            0
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
