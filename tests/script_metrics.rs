//! Script metrics, from a run through the real dispatch to the admin routes
//! an operator reads them from.
//!
//! The `interpreter_echo` fixture claims `lua`, so an XRPC query resolves its
//! script row and reaches the fixture as it would a deployed interpreter. Its
//! source is a directive: `host:script_log` makes one host import, and
//! `error:runtime` answers the error a script raising one would.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::app::TestApp;
use common::echo_interpreter;

const QUERY: &str = "games.gamesgamesgamesgames.listGames";

macro_rules! require_fixture {
    () => {
        if !echo_interpreter::is_built() {
            happyview::test_support::unavailable(echo_interpreter::BUILD);
            return;
        }
    };
}

fn trigger() -> String {
    format!("xrpc.query:{QUERY}")
}

async fn request(
    app: &TestApp,
    method: Method,
    uri: &str,
    auth: Option<(&str, String)>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    builder = match auth {
        Some((name, value)) => builder.header(name, value),
        None => {
            let cookie = app.admin_cookie();
            builder.header(cookie.0, cookie.1)
        }
    };
    let body = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        }
        None => Body::empty(),
    };
    let resp = app
        .router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn get(app: &TestApp, uri: &str) -> (StatusCode, Value) {
    request(app, Method::GET, uri, None, None).await
}

async fn seed_script(app: &TestApp, body: &str) {
    happyview::db::query(&happyview::db::adapt_sql(
        "DELETE FROM happyview_scripts WHERE id = ?",
        app.state.db_backend,
    ))
    .bind(trigger())
    .execute(&app.state.db)
    .await
    .unwrap();
    let now = happyview::db::now_rfc3339();
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_scripts (id, body, script_type, created_at, updated_at) \
         VALUES (?, ?, 'lua', ?, ?)",
        app.state.db_backend,
    ))
    .bind(trigger())
    .bind(body)
    .bind(&now)
    .bind(&now)
    .execute(&app.state.db)
    .await
    .expect("seed a script row");
}

async fn call_query(app: &TestApp) -> StatusCode {
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/xrpc/{QUERY}"))
                .header("x-client-key", "hvc_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn app_with_query() -> TestApp {
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
    let (status, body) = request(
        &app,
        Method::POST,
        "/admin/lexicons",
        None,
        Some(json!({
            "lexicon_json": common::fixtures::list_games_query_lexicon(),
            "target_collection": "games.gamesgamesgamesgames.game",
        })),
    )
    .await;
    assert!(status.is_success(), "{status}: {body}");
    app
}

fn row(listing: &Value) -> &Value {
    let scripts = listing["scripts"].as_array().expect("a scripts list");
    assert_eq!(scripts.len(), 1, "{listing:#}");
    &scripts[0]
}

#[tokio::test]
async fn a_run_is_counted_timed_and_read_back() {
    common::require_db!();
    require_fixture!();
    let app = app_with_query().await;

    seed_script(&app, "host:script_log").await;
    assert_eq!(call_query(&app).await, StatusCode::OK);

    let (status, listing) = get(&app, "/admin/metrics/scripts").await;
    assert_eq!(status, StatusCode::OK, "{listing}");
    println!("GET /admin/metrics/scripts after one run:\n{listing:#}");
    assert_eq!(listing["window"], "1h");
    assert_eq!(listing["enabled"], true);
    assert_eq!(listing["overflow"], 0);
    let script = row(&listing);
    assert_eq!(script["trigger"], trigger());
    assert_eq!(script["kind"], "xrpc_query");
    assert_eq!(script["layer"], "dashboard");
    assert_eq!(script["runs"], 1);
    assert_eq!(script["outcomes"], json!({"ok": 1}));
    assert!(script["wall_ms"]["p50"].as_f64().unwrap() > 0.0);
    assert!(script["guest_ms"]["max"].is_number());
    let instantiate = script["instantiate_ms_mean"].as_f64().unwrap();
    assert!(instantiate > 0.0);
    // Instantiation is building the linker and everything after it, so the
    // two parts add back up to it.
    let link = script["link_ms_mean"].as_f64().unwrap();
    let allocate = script["allocate_ms_mean"].as_f64().unwrap();
    assert!(link > 0.0 && allocate > 0.0, "{script:#}");
    assert!(
        (link + allocate - instantiate).abs() < 0.01,
        "{link} + {allocate} against {instantiate}"
    );
    assert!(script["teardown_ms_mean"].as_f64().unwrap() > 0.0);
    assert!(script["memory_peak_bytes"]["max"].as_u64().unwrap() > 0);
    // An installed interpreter's fuel says nothing, so none is reported.
    assert!(script["fuel_mean"].is_null());
    let cache = &script["module_cache"];
    assert_eq!(
        cache["hits"].as_u64().unwrap() + cache["misses"].as_u64().unwrap(),
        1
    );
    let imports = script["host_imports"].as_array().unwrap();
    let log = imports
        .iter()
        .find(|i| i["name"] == "host_script_log")
        .unwrap_or_else(|| panic!("host_script_log was not counted: {imports:?}"));
    assert_eq!(log["calls"], 1);

    seed_script(&app, "error:runtime").await;
    call_query(&app).await;
    let (_, listing) = get(&app, "/admin/metrics/scripts").await;
    let script = row(&listing);
    assert_eq!(script["runs"], 2);
    assert_eq!(script["outcomes"], json!({"ok": 1, "runtime": 1}));

    let encoded = trigger().replace(':', "%3A");
    let (status, series) = get(&app, &format!("/admin/metrics/scripts/{encoded}")).await;
    assert_eq!(status, StatusCode::OK, "{series}");
    let points = series["points"].as_array().unwrap();
    println!(
        "GET /admin/metrics/scripts/{encoded} (last point of {}):\n{:#}",
        points.len(),
        json!({
            "trigger": series["trigger"],
            "kind": series["kind"],
            "layers": series["layers"],
            "window": series["window"],
            "step": series["step"],
            "runs": series["runs"],
            "points": [points.last()],
        })
    );
    assert_eq!(series["trigger"], trigger());
    assert_eq!(series["step"], "1m");
    assert_eq!(series["runs"], 2);
    assert_eq!(points.len(), 60);
    let ran: u64 = points.iter().map(|p| p["runs"].as_u64().unwrap()).sum();
    assert_eq!(ran, 2);
    let last = points.last().unwrap();
    for field in ["link_ms_mean", "allocate_ms_mean", "teardown_ms_mean"] {
        assert!(last[field].as_f64().unwrap() > 0.0, "{field}: {last:#}");
    }

    let (status, series) = get(
        &app,
        &format!("/admin/metrics/scripts/{}?window=24h", trigger()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{series}");
    assert_eq!(series["step"], "30m");
    assert_eq!(series["points"].as_array().unwrap().len(), 48);

    let (status, _) = get(&app, "/admin/metrics/scripts/xrpc.query%3Anever.ran").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, live) = get(&app, "/admin/metrics/live").await;
    assert_eq!(status, StatusCode::OK, "{live}");
    println!("GET /admin/metrics/live:\n{live:#}");
    assert_eq!(live["enabled"], true);
    assert_eq!(
        live["active_runs"],
        json!({"xrpc_query": 0, "xrpc_procedure": 0, "record_event": 0, "label": 0, "job": 0})
    );
    assert_eq!(live["jobs"], json!({"pending": 0, "running": 0}));
    assert!(live["module_cache"]["compiles"].is_u64());

    // Switched off, a run leaves the windows as they were.
    let (status, body) = request(
        &app,
        Method::PUT,
        "/admin/settings/script_metrics_enabled",
        None,
        Some(json!({"value": "false"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    call_query(&app).await;
    let (_, listing) = get(&app, "/admin/metrics/scripts").await;
    assert_eq!(listing["enabled"], false);
    assert_eq!(row(&listing)["runs"], 2);

    let (status, _) = request(
        &app,
        Method::PUT,
        "/admin/settings/script_metrics_enabled",
        None,
        Some(json!({"value": "off"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn metrics_need_metrics_read_and_a_kept_window() {
    common::require_db!();
    let app = TestApp::new().await;
    let (status, body) = request(
        &app,
        Method::POST,
        "/admin/api-keys",
        None,
        Some(json!({ "name": "events", "permissions": ["events:read"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let key = body["key"].as_str().unwrap().to_string();
    for uri in [
        "/admin/metrics/scripts",
        "/admin/metrics/scripts/xrpc.query%3Aa.b.c",
        "/admin/metrics/live",
    ] {
        let (status, _) = request(
            &app,
            Method::GET,
            uri,
            Some(("authorization", format!("Bearer {key}"))),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }

    let (status, body) = get(&app, "/admin/metrics/scripts?window=7d").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.to_string().contains("1h"), "{body}");
    let (status, _) = get(&app, "/admin/metrics/scripts/x?window=24h&step=1m").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
