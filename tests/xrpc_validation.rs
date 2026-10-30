//! Lexicon validation of an XRPC call's parameters and input, driven through
//! the real routes.
//!
//! A call that breaks its method's lexicon answers `400 InvalidRequest` with
//! the atproto reference's wording, and the script never runs: the endpoints
//! here are bound to the `interpreter_echo` fixture, which would answer `200`
//! with whatever reached it. A call that keeps to the lexicon reaches the
//! script with its parameters coerced to their declared types.
//!
//! The same checks on a script calling another method, through
//! `xrpc.query` and `xrpc.procedure`, are pinned in `plugin_caller.rs`.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::app::TestApp;
use common::echo_interpreter;

const QUERY: &str = "com.example.validate.probe";
const PROCEDURE: &str = "com.example.validate.submit";
const DEFS: &str = "com.example.validate.defs";

/// Skips the caller when the fixture's module is absent, naming the build that
/// produces it.
macro_rules! require_fixture {
    () => {
        if !echo_interpreter::is_built() {
            eprintln!("skipping: {}", echo_interpreter::BUILD);
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// Lexicons
// ---------------------------------------------------------------------------

/// One parameter per constraint, so each test breaks exactly one.
fn query_lexicon() -> Value {
    json!({
        "lexicon": 1,
        "id": QUERY,
        "defs": {
            "main": {
                "type": "query",
                "parameters": {
                    "type": "params",
                    "required": ["q"],
                    "properties": {
                        "q": { "type": "string" },
                        "name": { "type": "string", "minLength": 2, "maxLength": 5 },
                        "emoji": { "type": "string", "maxGraphemes": 2 },
                        "sort": { "type": "string", "enum": ["new", "top"] },
                        "feed": { "type": "string", "knownValues": ["home", "trending"] },
                        "kind": { "type": "string", "const": "game" },
                        "actor": { "type": "string", "format": "did" },
                        "since": { "type": "string", "format": "datetime" },
                        "subject": { "type": "string", "format": "at-uri" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 50 },
                        "page": { "type": "integer", "enum": [1, 2, 3] },
                        "flag": { "type": "boolean" },
                        "tags": {
                            "type": "array",
                            "maxLength": 2,
                            "items": { "type": "string", "maxLength": 4 }
                        },
                        "ids": { "type": "array", "items": { "type": "integer" } }
                    }
                },
                "output": { "encoding": "application/json" }
            }
        }
    })
}

/// A procedure whose input reaches a local def, a def in another lexicon the
/// instance holds, and one in a lexicon it doesn't.
fn procedure_lexicon() -> Value {
    json!({
        "lexicon": 1,
        "id": PROCEDURE,
        "defs": {
            "main": {
                "type": "procedure",
                "parameters": {
                    "type": "params",
                    "properties": {
                        "dryRun": { "type": "boolean" }
                    }
                },
                "input": {
                    "encoding": "application/json",
                    "schema": {
                        "type": "object",
                        "required": ["text"],
                        "nullable": ["note"],
                        "properties": {
                            "text": { "type": "string", "maxLength": 5 },
                            "note": { "type": "string" },
                            "count": { "type": "integer", "minimum": 0 },
                            "mode": { "type": "string", "enum": ["draft", "live"] },
                            "at": { "type": "string", "format": "datetime" },
                            "labels": { "type": "array", "items": { "type": "string" } },
                            "inner": { "type": "ref", "ref": "#inner" },
                            "shared": { "type": "ref", "ref": format!("{DEFS}#thing") },
                            "elsewhere": { "type": "ref", "ref": "com.example.absent.defs#thing" },
                            "embed": {
                                "type": "union",
                                "closed": true,
                                "refs": ["#inner", format!("{DEFS}#thing")]
                            }
                        }
                    }
                },
                "output": { "encoding": "application/json" }
            },
            "inner": {
                "type": "object",
                "required": ["id"],
                "properties": { "id": { "type": "string" } }
            }
        }
    })
}

/// The lexicon `procedure_lexicon`'s `shared` ref lands in.
fn defs_lexicon() -> Value {
    json!({
        "lexicon": 1,
        "id": DEFS,
        "defs": {
            "thing": {
                "type": "object",
                "required": ["size"],
                "properties": { "size": { "type": "integer", "maximum": 10 } }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

async fn upload_lexicon(app: &TestApp, lexicon: Value) {
    let (status, body) = app
        .post_json_status("/admin/lexicons", json!({ "lexicon_json": lexicon }))
        .await;
    assert!(status < 300, "seeding a lexicon failed: {status} {body}");
}

async fn seed_echo_script(app: &TestApp, trigger_id: &str) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_scripts (id, body, script_type, created_at, updated_at) \
         VALUES (?, 'echo', 'lua', ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(trigger_id)
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed a script row");
}

/// Both endpoints, each bound to the echo fixture.
async fn app() -> TestApp {
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
    upload_lexicon(&app, defs_lexicon()).await;
    upload_lexicon(&app, query_lexicon()).await;
    upload_lexicon(&app, procedure_lexicon()).await;
    seed_echo_script(&app, &format!("xrpc.query:{QUERY}")).await;
    seed_echo_script(&app, &format!("xrpc.procedure:{PROCEDURE}")).await;
    app
}

async fn get(app: &TestApp, query_string: &str) -> (StatusCode, Value) {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/xrpc/{QUERY}?{query_string}"))
                .header("x-client-key", "hvc_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    (resp.status(), json_body(resp).await)
}

async fn post(app: &TestApp, query_string: &str, body: &Value) -> (StatusCode, Value) {
    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/xrpc/{PROCEDURE}?{query_string}"))
                .header(cookie.0, cookie.1)
                .header("x-client-key", "hvc_test")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    (resp.status(), json_body(resp).await)
}

fn assert_invalid((status, body): (StatusCode, Value), message: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "InvalidRequest", "{body}");
    assert_eq!(body["message"], message, "{body}");
}

/// The params the script was handed, from the echo of a call that got through.
fn sent_params((status, body): (StatusCode, Value)) -> Value {
    assert_eq!(status, StatusCode::OK, "{body}");
    body["input"].clone()
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_required_param_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "name=abc").await,
        "Params must have the property \"q\"",
    );
}

#[tokio::test]
async fn a_string_past_max_length_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "q=x&name=abcdef").await,
        "Params/name must not be longer than 5 characters",
    );
}

/// The reference counts a string's length in UTF-8 bytes: three letters,
/// six bytes.
#[tokio::test]
async fn max_length_counts_utf8_bytes() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "q=x&name=%C3%A9%C3%A9%C3%A9").await,
        "Params/name must not be longer than 5 characters",
    );
}

#[tokio::test]
async fn a_string_short_of_min_length_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "q=x&name=a").await,
        "Params/name must not be shorter than 2 characters",
    );
}

/// Two flags are four code points each, two graphemes: within the limit. A
/// third is not.
#[tokio::test]
async fn max_graphemes_counts_what_a_person_sees() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    let flag = "%F0%9F%87%AB%F0%9F%87%B7";
    let two = sent_params(get(&app, &format!("q=x&emoji={flag}{flag}")).await);
    assert_eq!(two["emoji"], "🇫🇷🇫🇷");
    assert_invalid(
        get(&app, &format!("q=x&emoji={flag}{flag}{flag}")).await,
        "Params/emoji must not be longer than 2 graphemes",
    );
}

#[tokio::test]
async fn a_value_outside_enum_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "q=x&sort=old").await,
        "Params/sort must be one of (new|top)",
    );
}

/// `knownValues` is a hint, not a constraint, in the reference too.
#[tokio::test]
async fn known_values_are_not_enforced() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    let sent = sent_params(get(&app, "q=x&feed=custom").await);
    assert_eq!(sent["feed"], "custom");
}

#[tokio::test]
async fn a_value_other_than_const_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "q=x&kind=movie").await,
        "Params/kind must be game",
    );
}

#[tokio::test]
async fn a_string_breaking_its_format_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "q=x&actor=alice").await,
        "Params/actor must be a valid did",
    );
    assert_invalid(
        get(&app, "q=x&since=yesterday").await,
        "Params/since must be a valid datetime (ISO 8601 with a timezone)",
    );
    assert_invalid(
        get(&app, "q=x&subject=https%3A%2F%2Fexample.com").await,
        "Params/subject must be a valid at-uri",
    );

    let sent = sent_params(
        get(
            &app,
            "q=x&actor=did%3Aplc%3Aabc123&since=2026-10-30T12%3A00%3A00Z\
             &subject=at%3A%2F%2Fdid%3Aplc%3Aabc123%2Fcom.example.post%2F3k2a",
        )
        .await,
    );
    assert_eq!(sent["actor"], "did:plc:abc123");
}

#[tokio::test]
async fn an_integer_out_of_range_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "q=x&limit=101").await,
        "Params/limit can not be greater than 100",
    );
    assert_invalid(
        get(&app, "q=x&limit=0").await,
        "Params/limit can not be less than 1",
    );
}

/// Today's `coerce_params` left an unparseable integer as a string for the
/// script to trip over.
#[tokio::test]
async fn a_param_that_is_not_an_integer_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "q=x&limit=ten").await,
        "Params/limit must be an integer",
    );
}

#[tokio::test]
async fn an_integer_outside_enum_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "q=x&page=4").await,
        "Params/page must be one of (1|2|3)",
    );
}

#[tokio::test]
async fn a_param_that_is_not_a_boolean_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "q=x&flag=maybe").await,
        "Params/flag must be a boolean",
    );
    let sent = sent_params(get(&app, "q=x&flag=true").await);
    assert_eq!(sent["flag"], true);
}

#[tokio::test]
async fn an_array_past_max_length_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "q=x&tags=a&tags=b&tags=c").await,
        "Params/tags must not have more than 2 elements",
    );
}

#[tokio::test]
async fn each_array_item_is_checked_against_items() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        get(&app, "q=x&tags=ok&tags=toolong").await,
        "Params/tags/1 must not be longer than 4 characters",
    );
    assert_invalid(
        get(&app, "q=x&ids=1&ids=two").await,
        "Params/ids/1 must be an integer",
    );
}

/// A parameter the lexicon declares as an array is one even when it was sent
/// once. It used to reach the script as a bare string.
#[tokio::test]
async fn a_single_value_for_an_array_param_arrives_as_an_array() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    let sent = sent_params(get(&app, "q=x&tags=solo&ids=7").await);
    assert_eq!(sent["tags"], json!(["solo"]));
    assert_eq!(sent["ids"], json!([7]));
}

#[tokio::test]
async fn a_missing_param_takes_its_default() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    let sent = sent_params(get(&app, "q=x").await);
    assert_eq!(sent["limit"], 50);
}

/// The reference ignores parameters its lexicon doesn't name, and so does
/// this: they reach the script untouched.
#[tokio::test]
async fn an_unknown_param_passes_through() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    let sent = sent_params(get(&app, "q=x&extra=123").await);
    assert_eq!(sent["extra"], "123");
}

/// A procedure's parameters are checked like a query's.
#[tokio::test]
async fn a_procedure_param_is_checked() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        post(&app, "dryRun=perhaps", &json!({ "text": "hi" })).await,
        "Params/dryRun must be a boolean",
    );
}

// ---------------------------------------------------------------------------
// Procedure input
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_input_that_keeps_to_the_schema_reaches_the_script() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    let input = json!({
        "text": "hello",
        "note": null,
        "count": 3,
        "mode": "live",
        "at": "2026-10-30T12:00:00.000Z",
        "labels": ["a", "b"],
        "inner": { "id": "x" },
        "shared": { "size": 4 },
        "embed": { "$type": format!("{PROCEDURE}#inner"), "id": "y" },
    });
    let (status, body) = post(&app, "", &input).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["input"], input);
}

#[tokio::test]
async fn an_input_missing_a_required_property_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        post(&app, "", &json!({ "count": 1 })).await,
        "Input must have the property \"text\"",
    );
}

#[tokio::test]
async fn an_input_that_is_not_an_object_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        post(&app, "", &json!(["text"])).await,
        "Input must be an object",
    );
}

#[tokio::test]
async fn an_input_property_breaking_its_constraint_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        post(&app, "", &json!({ "text": "too long" })).await,
        "Input/text must not be longer than 5 characters",
    );
    assert_invalid(
        post(&app, "", &json!({ "text": "hi", "count": -1 })).await,
        "Input/count can not be less than 0",
    );
    assert_invalid(
        post(&app, "", &json!({ "text": "hi", "mode": "secret" })).await,
        "Input/mode must be one of (draft|live)",
    );
    assert_invalid(
        post(&app, "", &json!({ "text": "hi", "at": "2026-10-30" })).await,
        "Input/at must be a valid datetime (ISO 8601 with a timezone)",
    );
}

#[tokio::test]
async fn an_input_property_of_the_wrong_type_is_refused() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        post(&app, "", &json!({ "text": 5 })).await,
        "Input/text must be a string",
    );
    assert_invalid(
        post(&app, "", &json!({ "text": "hi", "count": "3" })).await,
        "Input/count must be an integer",
    );
    assert_invalid(
        post(&app, "", &json!({ "text": "hi", "labels": "a" })).await,
        "Input/labels must be an array",
    );
}

#[tokio::test]
async fn null_is_refused_unless_the_property_is_nullable() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        post(&app, "", &json!({ "text": null })).await,
        "Input/text must be a string",
    );
}

#[tokio::test]
async fn a_local_ref_is_followed() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        post(&app, "", &json!({ "text": "hi", "inner": {} })).await,
        "Input/inner must have the property \"id\"",
    );
}

#[tokio::test]
async fn a_ref_into_another_held_lexicon_is_followed() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        post(&app, "", &json!({ "text": "hi", "shared": { "size": 11 } })).await,
        "Input/shared/size can not be greater than 10",
    );
}

/// A lexicon may point at definitions this instance never installed; those
/// aren't checked, and the call goes through.
#[tokio::test]
async fn a_ref_to_a_lexicon_the_instance_does_not_hold_is_skipped() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    let input = json!({ "text": "hi", "elsewhere": { "anything": ["goes"] } });
    let (status, body) = post(&app, "", &input).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["input"], input);
}

#[tokio::test]
async fn a_union_member_is_checked_against_its_type() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    assert_invalid(
        post(
            &app,
            "",
            &json!({ "text": "hi", "embed": { "$type": format!("{DEFS}#thing") } }),
        )
        .await,
        "Input/embed must have the property \"size\"",
    );
    assert_invalid(
        post(&app, "", &json!({ "text": "hi", "embed": { "id": "x" } })).await,
        "Input/embed must be an object which includes the \"$type\" property",
    );
}

#[tokio::test]
async fn a_closed_union_refuses_a_type_it_does_not_list() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    let (status, body) = post(
        &app,
        "",
        &json!({ "text": "hi", "embed": { "$type": "com.example.other" } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "InvalidRequest", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("Input/embed $type must be one of"),
        "{body}"
    );
}
