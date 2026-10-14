mod common;

use common::app::TestApp;
use happyview::error::AppError;
use happyview::lexicon_publishing::{Pattern, store};
use happyview::linked_repos::db as linked;
use serial_test::serial;

fn pat(s: &str) -> Pattern {
    Pattern::parse(s).unwrap()
}

#[tokio::test]
#[serial]
async fn rules_insert_list_update_delete() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;

    let rule = store::insert_rule(
        s,
        &pat("com.example.*"),
        "instance",
        None,
        false,
        "did:plc:a",
    )
    .await
    .unwrap();
    assert_eq!(rule.pattern, "com.example.*");
    assert!(!rule.auto_publish);
    assert_eq!(rule.parsed(), pat("com.example.*"));

    assert_eq!(store::list_rules(s).await.unwrap().len(), 1);
    assert_eq!(
        store::get_rule(s, &rule.id).await.unwrap().unwrap().id,
        rule.id
    );

    let updated = store::update_rule(s, &rule.id, None, None, Some(true))
        .await
        .unwrap()
        .unwrap();
    assert!(updated.auto_publish);
    assert_eq!(updated.target_kind, "instance");

    assert!(store::delete_rule(s, &rule.id).await.unwrap());
    assert!(!store::delete_rule(s, &rule.id).await.unwrap());
    assert!(
        store::update_rule(s, &rule.id, None, None, None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(store::list_rules(s).await.unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn duplicate_pattern_is_a_conflict() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;

    store::insert_rule(s, &pat("*"), "instance", None, false, "did:plc:a")
        .await
        .unwrap();
    let err = store::insert_rule(s, &pat("*"), "instance", None, true, "did:plc:a")
        .await
        .unwrap_err();
    match err {
        AppError::Conflict(msg) => assert_eq!(msg, "a default for * already exists"),
        other => panic!("expected conflict, got {other:?}"),
    }
}

#[tokio::test]
#[serial]
async fn deleting_the_linked_repo_keeps_the_rule_untargeted() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;

    let repo = linked::create(s, None, None, None, "atproto", "did:plc:a")
        .await
        .unwrap();
    let rule = store::insert_rule(
        s,
        &pat("com.example.*"),
        "linked_repo",
        Some(&repo.id),
        false,
        "did:plc:a",
    )
    .await
    .unwrap();
    assert_eq!(rule.linked_repo_id.as_deref(), Some(repo.id.as_str()));

    assert!(linked::delete(s, &repo.id).await.unwrap());

    let after = store::get_rule(s, &rule.id).await.unwrap().unwrap();
    assert_eq!(after.target_kind, "linked_repo");
    assert_eq!(after.linked_repo_id, None);

    let cleared = store::update_rule(s, &rule.id, None, Some(None), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cleared.linked_repo_id, None);
}

#[tokio::test]
#[serial]
async fn publication_failure_keeps_the_record_and_success_clears_the_error() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let nsid = "com.example.thing";

    store::record_failure(s, nsid, "boom").await.unwrap();
    let first = store::get_publication(s, nsid).await.unwrap().unwrap();
    assert_eq!(first.last_error.as_deref(), Some("boom"));
    assert!(first.uri.is_none() && first.published_at.is_none());
    assert!(first.last_attempt_at.is_some());

    store::record_success(s, nsid, "did:plc:a", "at://did:plc:a/x/y", "bafycid")
        .await
        .unwrap();
    let ok = store::get_publication(s, nsid).await.unwrap().unwrap();
    assert_eq!(ok.last_error, None);
    assert_eq!(ok.uri.as_deref(), Some("at://did:plc:a/x/y"));
    assert!(ok.published_at.is_some());

    store::record_failure(s, nsid, "later").await.unwrap();
    let failed = store::get_publication(s, nsid).await.unwrap().unwrap();
    assert_eq!(failed.last_error.as_deref(), Some("later"));
    assert_eq!(failed.uri.as_deref(), Some("at://did:plc:a/x/y"));
    assert_eq!(failed.cid.as_deref(), Some("bafycid"));
    assert_eq!(failed.did.as_deref(), Some("did:plc:a"));

    store::record_success(s, nsid, "did:plc:a", "at://did:plc:a/x/y", "bafycid2")
        .await
        .unwrap();
    assert_eq!(
        store::get_publication(s, nsid)
            .await
            .unwrap()
            .unwrap()
            .last_error,
        None
    );

    assert_eq!(store::list_publications(s).await.unwrap().len(), 1);
    assert!(store::delete_publication(s, nsid).await.unwrap());
    assert!(store::get_publication(s, nsid).await.unwrap().is_none());
}
