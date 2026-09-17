//! The `ctx.job` table a job script receives: its id, progress reporting,
//! cooperative cancellation and sleep. Built by the worker and handed to
//! `build_ctx`, which copies the canonical keys onto `ctx`.

use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
use std::sync::Arc;

use crate::AppState;
use crate::jobs;

pub fn job_ctx(lua: &Lua, state: Arc<AppState>, job_id: String) -> LuaResult<mlua::Table> {
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
                jobs::db::update_progress(&state, &job_id, &json_data)
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
                let result = jobs::db::should_stop(&state, &job_id).await;
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
    use crate::test_support::{memory_pool, test_state_with_pool};

    #[tokio::test]
    async fn job_ctx_carries_the_id_and_the_three_functions() {
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        let state = test_state_with_pool(memory_pool().await);
        let job = job_ctx(&lua, Arc::new(state), "test-id".into()).unwrap();
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
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        let state = test_state_with_pool(memory_pool().await);
        let counters = state.telemetry_counters.clone();
        let job = job_ctx(&lua, Arc::new(state), "test-id".into()).unwrap();
        lua.globals().set("job", job).unwrap();

        lua.load("job.wait(-5)").exec_async().await.unwrap();
        assert_eq!(
            counters
                .job_wait_ms
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }
}
