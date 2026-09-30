//! The `sdk_objects` fixture end to end: the object document an immediate
//! method receives, and each record/table host import called from wasm.
//!
//! Every assertion runs on each backend the suite can reach. `adapt_sql`'s
//! SQLite arm is a no-op, so a SQLite-only run exercises none of the
//! translation these imports depend on.

use happyview::db::{DatabaseBackend, adapt_sql};
use happyview::plugin::library::LibraryCallContext;
use happyview::plugin::loader;
use happyview::test_support::{migrated_memory_pool, test_state_from_env, test_state_with_pool_on};
use serde_json::{Value, json};
use serial_test::serial;

/// A collection and table of this file's own, so a shared database cannot mix
/// these rows with another suite's.
const COLLECTION: &str = "objects.fixture.record";
const DID: &str = "did:plc:objectsfixture";
const TABLE: &str = "objects_fixture_leaderboard";

fn uri(rkey: &str) -> String {
    format!("at://{DID}/{COLLECTION}/{rkey}")
}

/// Every backend the suite can reach, each with the fixture installed and the
/// rows seeded. The Postgres half needs a database to be pointed at and says
/// so when there is none.
async fn states() -> Vec<(happyview::AppState, &'static str)> {
    let mut out = vec![(
        state_on(migrated_memory_pool().await, DatabaseBackend::Sqlite).await,
        "sqlite",
    )];
    match test_state_from_env().await {
        Some(state) => {
            let name = match state.db_backend {
                DatabaseBackend::Sqlite => "sqlite",
                DatabaseBackend::Postgres => "postgres",
            };
            out.push((ready(state).await, name));
        }
        None => eprintln!("TEST_DATABASE_URL unset: the Postgres half is skipped"),
    }
    out
}

async fn state_on(pool: sqlx::AnyPool, backend: DatabaseBackend) -> happyview::AppState {
    ready(test_state_with_pool_on(pool, backend)).await
}

/// Seed the rows and install the fixture, so a state from either source
/// arrives in the same condition.
async fn ready(state: happyview::AppState) -> happyview::AppState {
    let plugin = loader::load_from_file(std::path::Path::new("tests/fixtures/sdk_objects"))
        .await
        .expect("sdk_objects fixture not built. Run: cargo build --manifest-path tests/fixtures/sdk_objects/Cargo.toml --target wasm32-unknown-unknown --release");
    seed(&state).await;
    state.plugin_registry.install(plugin).await.unwrap();
    state
}

async fn seed(state: &happyview::AppState) {
    let backend = state.db_backend;
    let db = &state.db;
    for sql in [
        "DELETE FROM happyview_record_refs WHERE collection = ?",
        "DELETE FROM happyview_records WHERE collection = ?",
    ] {
        happyview::db::query(&adapt_sql(sql, backend))
            .bind(COLLECTION)
            .execute(db)
            .await
            .expect("clear the collection");
    }

    let insert = adapt_sql(
        "INSERT INTO happyview_records \
         (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, NULL, ?)",
        backend,
    );
    for (rkey, body) in [
        (
            "1",
            json!({"n": "one", "score": 50, "author": {"handle": "alice"}}),
        ),
        (
            "2",
            json!({"n": "two", "score": 150, "author": {"handle": "bob"}}),
        ),
    ] {
        happyview::db::query(&insert)
            .bind(uri(rkey))
            .bind(DID)
            .bind(COLLECTION)
            .bind(rkey)
            .bind(body.to_string())
            .bind(format!("cid{rkey}"))
            .bind(format!("2026-01-01T00:00:0{rkey}+00:00"))
            .execute(db)
            .await
            .expect("seed a record");
    }
    happyview::db::query(&adapt_sql(
        "INSERT INTO happyview_record_refs (source_uri, target_uri, collection) VALUES (?, ?, ?)",
        backend,
    ))
    .bind(uri("2"))
    .bind(uri("1"))
    .bind(COLLECTION)
    .execute(db)
    .await
    .expect("seed a reference");

    happyview::db::query(&format!(
        "CREATE TABLE IF NOT EXISTS {TABLE} (name TEXT, score INTEGER)"
    ))
    .execute(db)
    .await
    .expect("create the table");
    happyview::db::query(&format!("DELETE FROM {TABLE}"))
        .execute(db)
        .await
        .expect("clear the table");
    happyview::db::query(&format!(
        "INSERT INTO {TABLE} (name, score) VALUES ('x', 50), ('y', 150)"
    ))
    .execute(db)
    .await
    .expect("seed the table");
}

async fn call(state: &happyview::AppState, function: &str, args: &[Value]) -> Value {
    state
        .plugin_executor()
        .call_library(
            "sdk_objects",
            function,
            args,
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_or_else(|e| panic!("{function} on {:?}: {e}", state.db_backend))
}

#[tokio::test]
#[serial]
async fn immediate_method_receives_the_whole_document() {
    let doc = json!({"args": [1], "steps": [{"add": [2]}, {"add": [3]}], "call": {"name": "doc", "args": []}});
    for (state, _) in states().await {
        let out = call(&state, "chain", std::slice::from_ref(&doc)).await;
        assert_eq!(out, doc);
    }
}

#[tokio::test]
#[serial]
async fn record_imports_round_trip_through_wasm() {
    for (state, backend_name) in states().await {
        let page = call(
            &state,
            "records_query",
            &[json!({"collection": COLLECTION, "limit": 1})],
        )
        .await;
        assert_eq!(page["records"][0]["uri"], uri("2"));
        assert_eq!(page["records"][0]["record"]["n"], json!("two"));
        assert!(page["cursor"].is_string());

        let n = call(
            &state,
            "records_count",
            &[json!({"collection": COLLECTION})],
        )
        .await;
        assert_eq!(n, json!(2));

        let got = call(&state, "records_get", &[json!(uri("1"))]).await;
        assert_eq!(
            got,
            json!({
                "uri": uri("1"),
                "did": DID,
                "collection": COLLECTION,
                "rkey": "1",
                "cid": "cid1",
                "indexed_at": null,
                "record": {"n": "one", "score": 50, "author": {"handle": "alice"}},
            })
        );

        let found = call(
            &state,
            "records_search",
            &[json!({"collection": COLLECTION, "field": "n", "query": "tw"})],
        )
        .await;
        assert_eq!(found.as_array().unwrap().len(), 1);
        assert_eq!(found[0]["uri"], uri("2"));
        assert_eq!(found[0]["record"]["n"], "two");

        // A dotted path is the case a hand-written Postgres JSON access gets
        // wrong, reading a top-level key of that literal name.
        let nested = call(
            &state,
            "records_search",
            &[json!({"collection": COLLECTION, "field": "author.handle", "query": "ali"})],
        )
        .await;
        assert_eq!(nested.as_array().unwrap().len(), 1);
        assert_eq!(nested[0]["uri"], uri("1"));

        let back = call(
            &state,
            "backlinks_query",
            &[json!({"uri": uri("1"), "collection": COLLECTION})],
        )
        .await;
        assert_eq!(back["records"][0]["uri"], uri("2"));
        assert_eq!(back["records"][0]["record"]["n"], "two");

        // A JSON string against an INTEGER column: the mismatch SQLite's
        // affinity hides and Postgres refuses to compare.
        let rows = call(
            &state,
            "table_query",
            &[json!({"table": TABLE, "filter": {"field": "score", "op": ">", "value": "100"}})],
        )
        .await;
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["name"], "y");

        // A record filter on a numeric field, which matches only where the
        // extracted value and the bound one are the same type.
        let filtered = call(
            &state,
            "records_query",
            &[json!({"collection": COLLECTION, "filter": {"field": "score", "op": "=", "value": 150}})],
        )
        .await;
        assert_eq!(filtered["records"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["records"][0]["uri"], uri("2"));

        let backend = call(&state, "backend", &[]).await;
        assert_eq!(backend, json!(backend_name));
    }
}

#[tokio::test]
#[serial]
async fn chain_through_lua_require() {
    for (state, _) in states().await {
        let lua = happyview::lua::sandbox_for_tests();
        happyview::lua::require_api_for_tests(&lua, &state).await;
        lua.load(format!(
            r#"local o = require("objects")
               function handle()
                 local d = o.chain("a"):add(1):doc()
                 local page = o.records_query({{ collection = "{COLLECTION}", limit = 1 }})
                 return d.args[1] .. ":" .. #d.steps .. ":" .. page.records[1].uri .. ":" .. page.records[1].record.n
               end"#
        ))
        .exec()
        .unwrap();
        let handle: mlua::Function = lua.globals().get("handle").unwrap();
        let out: String = handle.call_async(()).await.unwrap();
        assert_eq!(out, format!("a:1:{}:two", uri("2")));
    }
}

#[tokio::test]
#[serial]
async fn invalid_spec_is_a_plugin_error_not_a_trap() {
    for (state, _) in states().await {
        let err = state
            .plugin_executor()
            .call_library(
                "sdk_objects",
                "table_query",
                &[json!({"table": "happyview_plugin_secrets"})],
                &LibraryCallContext::default(),
                0,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("INVALID_SPEC"), "{err}");
    }
}
