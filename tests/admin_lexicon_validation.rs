//! The admin upload's answer when a lexicon does not validate: one entry per
//! field, so the editor can point at each of them.

mod common;

use common::app::TestApp;
use common::fixtures;
use serde_json::json;

#[tokio::test]
async fn an_invalid_upload_answers_one_issue_per_field() {
    let app = TestApp::new().await;

    // Three things wrong at once, in three different places.
    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({
                "lexicon_json": {
                    "lexicon": 1,
                    "id": "com.example.thing",
                    "defs": {
                        "main": {
                            "type": "record",
                            "key": "uuid",
                            "record": {
                                "type": "object",
                                "required": ["absent"],
                                "properties": {
                                    "a": { "type": "ref", "ref": "#missing" }
                                }
                            }
                        }
                    }
                },
                "backfill": false,
            }),
        )
        .await;

    assert_eq!(status, 400, "{body}");
    let issues = body["issues"].as_array().expect("issues array");
    let paths: Vec<&str> = issues
        .iter()
        .filter(|i| i["severity"] == "error")
        .map(|i| i["path"].as_str().unwrap_or_default())
        .collect();
    assert!(paths.contains(&"defs.main.key"), "{body}");
    assert!(
        paths.contains(&"defs.main.record.required[0]"),
        "the path should point at the offending entry: {body}"
    );
    assert!(
        paths.contains(&"defs.main.record.properties.a.ref"),
        "{body}"
    );
    for issue in issues {
        assert!(issue["message"].is_string(), "{issue}");
        assert!(
            issue["severity"] == "error" || issue["severity"] == "warning",
            "{issue}"
        );
    }
    // `error` still carries a sentence, for anything reading only that.
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("not valid")),
        "{body}"
    );

    // Nothing was stored.
    let listed = app.get_json("/admin/lexicons").await;
    assert!(
        !listed
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .any(|l| l["id"] == "com.example.thing"),
        "a refused upload must store nothing: {listed}"
    );

    // Warnings alone do not refuse: this one has a loose array and a def
    // name that is not camelCase, and it uploads.
    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({
                "lexicon_json": {
                    "lexicon": 1,
                    "id": "com.example.loose",
                    "defs": {
                        "main": {
                            "type": "record",
                            "key": "tid",
                            "record": {
                                "type": "object",
                                "properties": { "tags": { "type": "array" } }
                            }
                        },
                        "Odd_Name": { "type": "object" }
                    }
                },
                "backfill": false,
            }),
        )
        .await;
    assert_eq!(status, 201, "warnings are not refusals: {body}");
}

/// A `subscription` lexicon is valid, HappyView serves none, and it must not
/// succeed in silence: succeeding quietly is what makes it read as support.
#[tokio::test]
async fn an_accepted_subscription_lexicon_says_nothing_serves_it() {
    let app = TestApp::new().await;

    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({
                "lexicon_json": {
                    "lexicon": 1,
                    "id": "com.example.events",
                    "defs": {
                        "main": {
                            "type": "subscription",
                            "message": { "schema": { "type": "union", "refs": ["#evt"] } }
                        },
                        "evt": { "type": "object", "properties": { "seq": { "type": "integer" } } }
                    }
                },
                "backfill": false,
            }),
        )
        .await;

    assert_eq!(status, 201, "a valid subscription uploads: {body}");
    let issues = body["issues"]
        .as_array()
        .expect("the success response carries its warnings");
    assert_eq!(issues.len(), 1, "{body}");
    assert_eq!(issues[0]["severity"], "warning", "{body}");
    assert_eq!(issues[0]["path"], "defs.main.type", "{body}");
    let message = issues[0]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("does not serve subscriptions yet"),
        "{body}"
    );
    assert!(
        message.contains("no endpoint will be generated"),
        "it has to say what will happen: {body}"
    );

    // A lexicon HappyView does serve carries no warnings, or every upload
    // would arrive with noise attached.
    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({ "lexicon_json": fixtures::game_record_lexicon(), "backfill": false }),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(
        body["issues"].as_array().map(Vec::len),
        Some(0),
        "a record lexicon should warn about nothing: {body}"
    );
}
