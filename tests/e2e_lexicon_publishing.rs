mod common;

use common::app::TestApp;
use happyview::error::AppError;
use happyview::lexicon_publishing::{
    Pattern, instance_target, linked_repo_target, list_targets, publish, publishable, reconcile,
    store, target_for_rule,
};
use happyview::linked_repos::db as linked;
use happyview::linked_repos::types::{
    LinkedRepo, STATUS_ACTIVE, STATUS_NEEDS_REAUTH, STATUS_PENDING,
};
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

const FULL_SCOPE: &str =
    "atproto repo:com.atproto.lexicon.schema?action=create&action=update&action=delete";

fn record_lexicon(nsid: &str) -> serde_json::Value {
    serde_json::json!({
        "lexicon": 1,
        "id": nsid,
        "defs": {"main": {"type": "record", "key": "tid", "record": {"type": "object", "properties": {}}}},
    })
}

async fn put_manual(s: &happyview::AppState, nsid: &str) {
    s.lexicons
        .put_manual(
            &s.db,
            s.db_backend,
            happyview::catalog::LexiconWrite {
                lexicon_json: record_lexicon(nsid),
                backfill: false,
                target_collection: None,
                action: happyview::lexicon::ProcedureAction::Upsert,
                token_cost: None,
                authority_did: None,
            },
        )
        .await
        .unwrap();
}

async fn active_grant(s: &happyview::AppState, did: &str, scopes: &str) -> LinkedRepo {
    let repo = linked::create(s, None, None, None, scopes, "did:plc:admin")
        .await
        .unwrap();
    linked::bind_did(s, &repo.id, did, None).await.unwrap();
    linked::get(s, &repo.id).await.unwrap().unwrap()
}

async fn count_events(s: &happyview::AppState, event_type: &str, subject: &str) -> i64 {
    let sql = happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = ? AND subject = ?",
        s.db_backend,
    );
    let (n,): (i64,) = happyview::db::query_as(&sql)
        .bind(event_type)
        .bind(subject)
        .fetch_one(&s.db)
        .await
        .unwrap();
    n
}

fn grant(did: Option<&str>, status: &str, scopes: &str) -> LinkedRepo {
    LinkedRepo {
        id: "g1".into(),
        did: did.map(str::to_string),
        handle: Some("a.test".into()),
        reason: None,
        scopes: scopes.into(),
        status: status.into(),
        last_error: None,
        last_refreshed_at: None,
        authorized_at: None,
        created_by: "did:plc:admin".into(),
        created_at: "2026-10-13T00:00:00+00:00".into(),
    }
}

#[test]
fn linked_repo_target_reads_the_grant_status_and_scopes() {
    let pending = linked_repo_target(&grant(None, STATUS_PENDING, FULL_SCOPE));
    assert!(!pending.available);
    assert_eq!(
        pending.unavailable_reason.as_deref(),
        Some("this linked repo has not been authorized yet")
    );

    let reauth = linked_repo_target(&grant(Some("did:plc:a"), STATUS_NEEDS_REAUTH, FULL_SCOPE));
    assert!(!reauth.available && !reauth.can_unpublish);
    assert_eq!(
        reauth.unavailable_reason.as_deref(),
        Some("this linked repo needs to be reauthorized")
    );

    let create_only = linked_repo_target(&grant(
        Some("did:plc:a"),
        STATUS_ACTIVE,
        "atproto repo:com.atproto.lexicon.schema?action=create",
    ));
    assert!(!create_only.available);
    assert!(
        create_only
            .unavailable_reason
            .unwrap()
            .contains("repo:com.atproto.lexicon.schema?action=create&action=update")
    );

    let no_delete = linked_repo_target(&grant(
        Some("did:plc:a"),
        STATUS_ACTIVE,
        "atproto repo:com.atproto.lexicon.schema?action=create&action=update",
    ));
    assert!(no_delete.available && !no_delete.can_unpublish);

    let full = linked_repo_target(&grant(Some("did:plc:a"), STATUS_ACTIVE, FULL_SCOPE));
    assert!(full.available && full.can_unpublish);
    assert_eq!(full.unavailable_reason, None);
    assert_eq!(full.kind, "linked_repo");
    assert_eq!(full.linked_repo_id.as_deref(), Some("g1"));
    assert_eq!(full.did.as_deref(), Some("did:plc:a"));
    assert_eq!(full.handle.as_deref(), Some("a.test"));
}

#[tokio::test]
#[serial]
async fn instance_target_names_did_web_as_the_reason_it_is_unavailable() {
    common::require_db!();
    let mut app = TestApp::new().await;

    let unset = instance_target(&app.state).await.unwrap();
    assert!(!unset.available);
    assert_eq!(unset.kind, "instance");

    app.setup_did_web().await;
    let target = instance_target(&app.state).await.unwrap();
    assert!(!target.available && !target.can_unpublish);
    assert!(target.unavailable_reason.unwrap().contains("did:web"));

    let targets = list_targets(&app.state).await.unwrap();
    assert_eq!(targets[0].kind, "instance");
}

#[tokio::test]
#[serial]
async fn publish_without_a_rule_records_the_failure() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    put_manual(s, "com.example.norule").await;

    let outcome = publish(s, "com.example.norule", Some("did:plc:admin")).await;
    assert!(!outcome.ok);
    assert_eq!(
        outcome.error.as_deref(),
        Some("no publishing default covers com.example.norule")
    );
    let row = store::get_publication(s, "com.example.norule")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.last_error.as_deref(),
        Some("no publishing default covers com.example.norule")
    );
    assert_eq!(row.did, None);
    assert_eq!(
        count_events(s, "lexicon.publish_failed", "com.example.norule").await,
        1
    );
}

#[tokio::test]
#[serial]
async fn publish_refuses_a_network_lexicon_even_when_a_rule_matches() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let nsid = "com.other.thing";
    s.lexicons
        .put_network(
            &s.db,
            s.db_backend,
            happyview::catalog::LexiconWrite {
                lexicon_json: record_lexicon(nsid),
                backfill: false,
                target_collection: None,
                action: happyview::lexicon::ProcedureAction::Upsert,
                token_cost: None,
                authority_did: Some("did:plc:other".into()),
            },
        )
        .await
        .unwrap();
    let repo = active_grant(s, "did:plc:repoa", FULL_SCOPE).await;
    store::insert_rule(
        s,
        &pat("*"),
        "linked_repo",
        Some(&repo.id),
        true,
        "did:plc:admin",
    )
    .await
    .unwrap();

    assert!(publishable(s).await.unwrap().iter().all(|l| l.id != nsid));
    let outcome = publish(s, nsid, None).await;
    assert_eq!(
        outcome.error.as_deref(),
        Some("not a lexicon HappyView can publish")
    );
    assert!(store::get_publication(s, nsid).await.unwrap().is_none());
}

#[tokio::test]
#[serial]
async fn publish_to_a_grant_needing_reauth_records_the_error() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    put_manual(s, "com.example.reauth").await;
    let repo = active_grant(s, "did:plc:repoa", FULL_SCOPE).await;
    linked::mark_needs_reauth(s, &repo.id, "token revoked")
        .await
        .unwrap();
    store::insert_rule(
        s,
        &pat("com.example.*"),
        "linked_repo",
        Some(&repo.id),
        false,
        "did:plc:admin",
    )
    .await
    .unwrap();

    let outcome = publish(s, "com.example.reauth", None).await;
    assert!(!outcome.ok);
    assert_eq!(
        outcome.error.as_deref(),
        Some("this linked repo needs to be reauthorized")
    );
    let row = store::get_publication(s, "com.example.reauth")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.last_error.as_deref(),
        Some("this linked repo needs to be reauthorized")
    );
}

#[tokio::test]
#[serial]
async fn a_rule_whose_linked_repo_was_removed_never_falls_back_to_a_broader_rule() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let nsid = "com.example.orphan";
    put_manual(s, nsid).await;

    let chosen = active_grant(s, "did:plc:chosen", FULL_SCOPE).await;
    let fallback = active_grant(s, "did:plc:fallback", FULL_SCOPE).await;
    let rule = store::insert_rule(
        s,
        &pat("com.example.*"),
        "linked_repo",
        Some(&chosen.id),
        true,
        "did:plc:admin",
    )
    .await
    .unwrap();
    store::insert_rule(
        s,
        &pat("*"),
        "linked_repo",
        Some(&fallback.id),
        true,
        "did:plc:admin",
    )
    .await
    .unwrap();
    assert!(linked::delete(s, &chosen.id).await.unwrap());

    let rule = store::get_rule(s, &rule.id).await.unwrap().unwrap();
    let grants = linked::list(s).await.unwrap();
    let instance = instance_target(s).await.unwrap();
    let target = target_for_rule(s, &rule, &grants, &instance).await;
    assert!(!target.available);
    assert_eq!(target.kind, "linked_repo");
    assert_eq!(target.did, None);
    assert_eq!(
        target.unavailable_reason.as_deref(),
        Some("the linked repo this default used was removed")
    );

    let outcome = publish(s, nsid, None).await;
    assert_eq!(
        outcome.error.as_deref(),
        Some("the linked repo this default used was removed")
    );

    // The fallback grant would have been marked needs_reauth had anything
    // tried to write through it.
    reconcile(s.clone()).await;
    let fallback = linked::get(s, &fallback.id).await.unwrap().unwrap();
    assert_eq!(fallback.status, STATUS_ACTIVE);
    assert_eq!(count_events(s, "lexicon.publish_failed", nsid).await, 1);
}

#[tokio::test]
#[serial]
async fn reconcile_without_auto_rules_attempts_nothing() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    put_manual(s, "com.example.manualonly").await;
    let repo = active_grant(s, "did:plc:repoa", FULL_SCOPE).await;
    store::insert_rule(
        s,
        &pat("*"),
        "linked_repo",
        Some(&repo.id),
        false,
        "did:plc:admin",
    )
    .await
    .unwrap();

    reconcile(s.clone()).await;

    assert!(store::list_publications(s).await.unwrap().is_empty());
    assert_eq!(
        count_events(s, "lexicon.publish_failed", "com.example.manualonly").await,
        0
    );
    assert_eq!(
        linked::get(s, &repo.id).await.unwrap().unwrap().status,
        STATUS_ACTIVE
    );
}

#[tokio::test]
#[serial]
async fn concurrent_reconciles_complete_and_attempt_each_lexicon_once() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let nsid = "com.example.auto";
    put_manual(s, nsid).await;
    // Active with no stored session: the write fails at session restore and
    // marks the grant needs_reauth, so any later run finds nothing due.
    let repo = active_grant(s, "did:plc:repoa", FULL_SCOPE).await;
    store::insert_rule(
        s,
        &pat("*"),
        "linked_repo",
        Some(&repo.id),
        true,
        "did:plc:admin",
    )
    .await
    .unwrap();

    tokio::join!(reconcile(s.clone()), reconcile(s.clone()));

    let row = store::get_publication(s, nsid).await.unwrap().unwrap();
    let error = row.last_error.unwrap();
    assert!(error.contains("needs reauthorization"), "{error}");
    assert_eq!(row.did, None);
    assert_eq!(
        linked::get(s, &repo.id).await.unwrap().unwrap().status,
        STATUS_NEEDS_REAUTH
    );
    assert_eq!(count_events(s, "lexicon.publish_failed", nsid).await, 1);
}
