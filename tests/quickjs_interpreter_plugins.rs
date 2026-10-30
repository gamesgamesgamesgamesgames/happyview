//! The real JavaScript and TypeScript interpreter plugins, loaded and run by
//! the host: a prepared artifact run without its source, a local module
//! resolved through `host_module_resolve` against a snapshot, and the
//! cross-language rule that a script reaches only files in its own language.
//!
//! ```sh
//! HAPPYVIEW_JAVASCRIPT_PLUGIN=<dir> HAPPYVIEW_TYPESCRIPT_PLUGIN=<dir> \
//!   cargo test --test quickjs_interpreter_plugins
//! ```

mod common;

use std::sync::Arc;

use serde_json::json;

use common::app::TestApp;
use common::lua_plugin::identify;
use common::quickjs_plugins::{JAVASCRIPT_DIR, TYPESCRIPT_DIR, plugin_dir};
use happyview::plugin::{
    ChunkRole, ModuleScope, PreparedArtifact, ScriptErrorKind, ScriptExecuteContext,
    ScriptExecuteInput, ScriptExecuteLimits, ScriptExecuteOutput, ScriptKind, ScriptValueKind,
    loader,
};
use happyview::project::snapshot::{ProjectModule, ProjectSnapshot};

async fn register(app: &TestApp, var: &str) -> Option<String> {
    let dir = plugin_dir(var)?;
    println!("module: {}", identify(&dir));
    let plugin = loader::load_from_file(&dir)
        .await
        .expect("the plugin should load through the loader");
    let id = plugin.info.id.clone();
    app.state.plugin_registry.register(plugin).await;
    Some(id)
}

fn input(source: &str, chunk_name: &str) -> ScriptExecuteInput {
    ScriptExecuteInput {
        source: source.to_string(),
        chunk_name: chunk_name.to_string(),
        artifact: None,
        kind: ScriptKind::XrpcQuery,
        input: json!({ "q": "hi" }),
        context: ScriptExecuteContext {
            trigger: "xrpc.query:app.test.q".into(),
            ..ScriptExecuteContext::default()
        },
        libraries: Vec::new(),
        limits: ScriptExecuteLimits {
            instructions: Some(1_000_000),
            memory_bytes: 64 * 1024 * 1024,
        },
        removed_globals: Vec::new(),
    }
}

fn module(
    id: &str,
    language_id: &str,
    source: &str,
    artifact: Option<PreparedArtifact>,
) -> (String, ProjectModule) {
    (
        id.to_string(),
        ProjectModule {
            id: id.to_string(),
            path: id.into(),
            language_id: language_id.to_string(),
            source: source.into(),
            artifact,
        },
    )
}

fn in_project(snapshot: &Arc<ProjectSnapshot>) -> ModuleScope {
    ModuleScope::Project(snapshot.clone())
}

#[tokio::test]
async fn a_typescript_artifact_runs_without_its_source_on_typescript_lines() {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(ts) = register(&app, TYPESCRIPT_DIR).await else {
        return;
    };
    let executor = app.state.plugin_executor();
    let name = "src/lexicons/app/test/q.ts";
    let prepared = executor
        .prepare_script(
            &ts,
            "enum Color {\n  Red,\n  Green,\n}\nexport default function handle(input: { q: string }): string {\n  if (input.q === 'boom') throw new Error('boom');\n  return input.q + Color.Green;\n}\n",
            name,
            ChunkRole::Handler,
        )
        .await
        .expect("validate should answer");
    let artifact = prepared.artifact.expect("an artifact");
    assert_eq!(artifact.format, "quickjs-bytecode");
    assert!(artifact.source_map.is_some());

    let mut sent = input("", name);
    sent.artifact = Some(artifact);
    assert_eq!(
        executor.execute_script(&ts, &sent).await.unwrap(),
        ScriptExecuteOutput::Returned {
            value: json!("hi1"),
            value_kind: ScriptValueKind::Other
        }
    );
    sent.input = json!({ "q": "boom" });
    match executor.execute_script(&ts, &sent).await.unwrap() {
        ScriptExecuteOutput::Error { chunk, line, .. } => {
            assert_eq!(chunk.as_deref(), Some(name));
            assert_eq!(line, Some(6));
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[tokio::test]
async fn a_script_reaches_only_modules_in_its_own_language() {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(js) = register(&app, JAVASCRIPT_DIR).await else {
        return;
    };
    let Some(ts) = register(&app, TYPESCRIPT_DIR).await else {
        return;
    };
    let executor = app.state.plugin_executor();

    let fmt_id = "src/lexicons/app/test/lib/fmt.ts";
    let fmt_source = "export function shout(s: string): string {\n  return s.toUpperCase();\n}\n";
    let fmt = executor
        .prepare_script(&ts, fmt_source, fmt_id, ChunkRole::Module)
        .await
        .unwrap();
    assert!(fmt.valid, "{:?}", fmt.errors);
    let mut snapshot = ProjectSnapshot::empty();
    snapshot
        .modules
        .extend([module(fmt_id, "typescript", fmt_source, fmt.artifact)]);
    let snapshot = Arc::new(snapshot);

    // TypeScript reaches it, without and with the extension.
    for specifier in ["./lib/fmt", "./lib/fmt.ts"] {
        let output = executor
            .execute_script_in(
                &ts,
                &input(
                    &format!("import {{ shout }} from '{specifier}';\nexport default (input: {{ q: string }}) => shout(input.q);"),
                    "src/lexicons/app/test/q.ts",
                ),
                None,
                in_project(&snapshot),
            )
            .await
            .unwrap();
        assert_eq!(
            output,
            ScriptExecuteOutput::Returned {
                value: json!("HI"),
                value_kind: ScriptValueKind::Other
            },
            "{specifier}"
        );
    }

    // JavaScript does not, whichever way it asks, and the host's sentence
    // names the TypeScript file that is there.
    for specifier in ["./lib/fmt", "./lib/fmt.ts"] {
        let output = executor
            .execute_script_in(
                &js,
                &input(
                    &format!("import {{ shout }} from '{specifier}';\nexport default (input) => shout(input.q);"),
                    "src/lexicons/app/test/q.js",
                ),
                None,
                in_project(&snapshot),
            )
            .await
            .unwrap();
        match output {
            ScriptExecuteOutput::Error { kind, message, .. } => {
                assert_eq!(kind, ScriptErrorKind::Runtime, "{specifier}");
                assert!(message.contains(specifier), "{specifier}: {message}");
                assert!(message.contains("lib/fmt.ts"), "{specifier}: {message}");
            }
            other => panic!("{specifier}: expected a failure, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn validate_reports_imports_for_the_host_to_check() {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(js) = register(&app, JAVASCRIPT_DIR).await else {
        return;
    };
    let prepared = app
        .state
        .plugin_executor()
        .prepare_script(
            &js,
            "import { a } from './lib/a.js';\nexport default async () => (await import('../b.js')).b + a;",
            "src/lexicons/app/test/q.js",
            ChunkRole::Handler,
        )
        .await
        .unwrap();
    let found: Vec<(String, u32)> = prepared
        .imports
        .into_iter()
        .map(|import| (import.specifier, import.line))
        .collect();
    assert_eq!(found, vec![("./lib/a.js".into(), 1), ("../b.js".into(), 2)]);
}
