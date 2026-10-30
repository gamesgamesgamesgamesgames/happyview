//! The admin upload's answer when a lexicon does not validate: one entry per
//! field, so the editor can point at each of them.

mod common;

use common::app::TestApp;
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
