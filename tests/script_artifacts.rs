//! What a run is handed: the artifact its interpreter prepared, instead of
//! the source, and the module scope its imports resolve in. Driven through
//! `script::dispatch`, the one path every runner takes, with the echo
//! fixture reporting what it received.

mod common;

use std::sync::Arc;

use serde_json::{Value, json};

use common::app::TestApp;
use common::echo_interpreter;
use happyview::plugin::{ChunkRole, PreparedArtifact, ScriptExecuteOutput};
use happyview::project::snapshot::{ProjectModule, ProjectScript, ProjectSnapshot, ScriptBody};
use happyview::script::{Invocation, ScriptOrigin, Trigger, dispatch};

const ECHO: &str = "interpreter_echo";

async fn app() -> Option<TestApp> {
    if std::env::var("TEST_DATABASE_URL").is_err() {
        happyview::test_support::unavailable(happyview::test_support::NO_DATABASE);
        return None;
    }
    if !echo_interpreter::is_built() {
        happyview::test_support::unavailable(echo_interpreter::BUILD);
        return None;
    }
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin_with_extensions("lua", &["lua"]))
        .await;
    Some(app)
}

fn invocation<'a>(source: &'a str, input: &'a Value, origin: ScriptOrigin<'a>) -> Invocation<'a> {
    Invocation {
        trigger_id: "job.run:tick",
        trigger: Trigger::Job { id: "job-1" },
        language: "lua",
        source,
        input,
        caller_did: None,
        has_pds_auth: false,
        method: None,
        collection: None,
        params: None,
        delegate_did: None,
        space: None,
        origin,
    }
}

async fn sent(app: &TestApp, inv: &Invocation<'_>) -> Value {
    match dispatch(&app.state, inv, None).await.expect("dispatch") {
        ScriptExecuteOutput::Returned { value, .. } => value,
        other => panic!("expected a returned value, got {other:?}"),
    }
}

/// An artifact the registered echo build prepared from `source`, stamped as
/// that build's.
async fn prepared(app: &TestApp, source: &str, chunk: &str) -> PreparedArtifact {
    app.state
        .plugin_executor()
        .prepare_script(ECHO, source, chunk, ChunkRole::Handler)
        .await
        .expect("prepare")
        .artifact
        .expect("the echo fixture prepares an artifact")
}

#[tokio::test]
async fn a_dashboard_run_is_prepared_once_and_then_loaded_from_the_cache() {
    let Some(app) = app().await else { return };
    let input = json!({});
    let inv = invocation("echo me", &input, ScriptOrigin::Dashboard);

    let first = sent(&app, &inv).await;
    assert_eq!(first["source"], "", "{first}");
    assert_eq!(first["artifact"]["format"], "echo-source");
    assert_eq!(first["chunk_name"], "script");
    assert_eq!(echo_interpreter::script_text(&first), "echo me");

    let cached = app
        .state
        .plugin_registry
        .prepared()
        .lookup(ECHO, "echo me")
        .expect("the first run fills the cache")
        .expect("with an artifact");
    assert_eq!(cached.bytes, b"echo me");

    // Swap what the cache holds for this source. A second run that prepared
    // again would load "echo me"; one that reads the cache runs this.
    let module = app.state.plugin_registry.get(ECHO).await.unwrap();
    let mut swapped = (*cached).clone();
    swapped.bytes = br#"returns:"cached""#.to_vec();
    app.state
        .plugin_registry
        .store_prepared(&module, "echo me", Some(swapped))
        .expect("the registered build stores");
    assert_eq!(sent(&app, &inv).await, json!("cached"));
}

/// An interpreter that cannot prepare is remembered as such, so its scripts
/// run from source without a second validate on every run.
#[tokio::test]
async fn an_interpreter_that_returns_no_artifact_is_asked_once() {
    let Some(app) = app().await else { return };
    let input = json!({});
    let inv = invocation("no-artifact here", &input, ScriptOrigin::Dashboard);

    let first = sent(&app, &inv).await;
    assert_eq!(first["source"], "no-artifact here");
    assert!(first.get("artifact").is_none(), "{first}");
    assert_eq!(
        app.state
            .plugin_registry
            .prepared()
            .lookup(ECHO, "no-artifact here"),
        Some(None)
    );

    // A body the fixture would prepare, already known to have no artifact:
    // a run that asked again would be handed one.
    let module = app.state.plugin_registry.get(ECHO).await.unwrap();
    app.state
        .plugin_registry
        .store_prepared(&module, "echo me", None);
    let inv = invocation("echo me", &input, ScriptOrigin::Dashboard);
    let run = sent(&app, &inv).await;
    assert_eq!(run["source"], "echo me", "{run}");
    assert!(run.get("artifact").is_none(), "{run}");
}

/// The cache is keyed by the source, so an edited script is a miss and its
/// new text runs rather than the artifact of the old.
#[tokio::test]
async fn an_edited_dashboard_script_runs_its_new_source() {
    let Some(app) = app().await else { return };
    let input = json!({});
    let before = invocation(r#"returns:"before""#, &input, ScriptOrigin::Dashboard);
    assert_eq!(sent(&app, &before).await, json!("before"));

    let after = invocation(r#"returns:"after""#, &input, ScriptOrigin::Dashboard);
    assert_eq!(sent(&app, &after).await, json!("after"));
}

fn project_script(path: &str, source: &str, artifact: Option<PreparedArtifact>) -> ProjectScript {
    ProjectScript {
        trigger: "job.run:tick".into(),
        path: path.into(),
        language_id: "lua".into(),
        body: ScriptBody::Source {
            source: Arc::from(source),
            artifact,
        },
        description: None,
        outbound_xrpcs: Vec::new(),
    }
}

#[tokio::test]
async fn a_project_run_carries_its_path_its_artifact_and_its_snapshot() {
    let Some(app) = app().await else { return };
    let artifact = prepared(&app, "host:module_resolve", "src/jobs/tick.lua").await;
    let mut snap = ProjectSnapshot::empty();
    snap.modules.insert(
        "src/lib/format.lua".into(),
        ProjectModule {
            id: "src/lib/format.lua".into(),
            path: "src/lib/format.lua".into(),
            language_id: "lua".into(),
            source: Arc::from("return {}"),
            artifact: None,
        },
    );
    let snap = Arc::new(snap);
    let script = project_script("src/jobs/tick.lua", "host:module_resolve", Some(artifact));
    let input = json!({"specifier": "../lib/format"});
    let inv = invocation(
        "host:module_resolve",
        &input,
        ScriptOrigin::Project {
            snapshot: &snap,
            script: &script,
        },
    );

    // The fixture ran the artifact's directive, with the run's chunk as the
    // referrer, against the snapshot the origin named.
    let value = sent(&app, &inv).await;
    assert_eq!(value["ok"]["id"], "src/lib/format.lua", "{value}");
}

#[tokio::test]
async fn a_stale_project_artifact_is_withheld_and_the_source_runs() {
    let Some(app) = app().await else { return };
    let stale = PreparedArtifact {
        interpreter: ECHO.into(),
        version: "0.0.1+0000000000000000".into(),
        engine: "echo-1".into(),
        format: "echo-source".into(),
        bytes: br#"returns:"stale""#.to_vec(),
        source_map: None,
    };
    let snap = Arc::new(ProjectSnapshot::empty());
    let script = project_script("src/jobs/tick.lua", r#"returns:"fresh""#, Some(stale));
    let input = json!({});
    let inv = invocation(
        r#"returns:"fresh""#,
        &input,
        ScriptOrigin::Project {
            snapshot: &snap,
            script: &script,
        },
    );
    assert_eq!(sent(&app, &inv).await, json!("fresh"));
}

/// Two interpreters can share an engine and a version string, as the JS and
/// TS plugins do; the plugin id is what keeps one's artifact from the other.
#[tokio::test]
async fn an_artifact_another_interpreter_prepared_is_withheld() {
    let Some(app) = app().await else { return };
    let mut theirs = prepared(&app, r#"returns:"theirs""#, "src/jobs/tick.lua").await;
    theirs.interpreter = "interpreter_other".into();
    let snap = Arc::new(ProjectSnapshot::empty());
    let script = project_script("src/jobs/tick.lua", r#"returns:"ours""#, Some(theirs));
    let input = json!({});
    let inv = invocation(
        r#"returns:"ours""#,
        &input,
        ScriptOrigin::Project {
            snapshot: &snap,
            script: &script,
        },
    );
    assert_eq!(sent(&app, &inv).await, json!("ours"));
}

/// An artifact goes only with the text it was prepared from: a run whose
/// source differs from the one beside the artifact runs its own source.
#[tokio::test]
async fn a_project_artifact_never_runs_in_place_of_other_source() {
    let Some(app) = app().await else { return };
    let old = prepared(&app, r#"returns:"old""#, "src/jobs/tick.lua").await;
    let snap = Arc::new(ProjectSnapshot::empty());
    let script = project_script("src/jobs/tick.lua", r#"returns:"old""#, Some(old));
    let input = json!({});
    let inv = invocation(
        r#"returns:"new""#,
        &input,
        ScriptOrigin::Project {
            snapshot: &snap,
            script: &script,
        },
    );
    assert_eq!(sent(&app, &inv).await, json!("new"));
}
