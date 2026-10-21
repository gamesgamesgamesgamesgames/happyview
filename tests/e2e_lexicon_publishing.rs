mod common;

use common::app::TestApp;
use happyview::error::AppError;
use happyview::lexicon_publishing::{
    LinkedRepoTargets, Pattern, instance_target, linked_repo_target, list_targets, publish,
    publishable, reconcile, store, target_for_rule, unpublish,
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
        AppError::Conflict(msg) => assert_eq!(msg, "a rule for * already exists"),
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
}

#[tokio::test]
#[serial]
async fn withdrawal_forgets_the_record_and_a_success_clears_it() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let nsid = "com.example.withdrawn";

    store::record_success(s, nsid, "did:plc:a", "at://did:plc:a/x/y", "bafycid")
        .await
        .unwrap();
    assert_eq!(
        store::get_publication(s, nsid)
            .await
            .unwrap()
            .unwrap()
            .withdrawn_at,
        None
    );

    store::record_withdrawn(s, nsid).await.unwrap();
    let withdrawn = store::get_publication(s, nsid).await.unwrap().unwrap();
    assert!(withdrawn.withdrawn_at.is_some());
    assert_eq!(withdrawn.did, None);
    assert_eq!(withdrawn.uri, None);
    assert_eq!(withdrawn.cid, None);
    assert_eq!(withdrawn.published_at, None);
    assert_eq!(withdrawn.last_error, None);

    store::record_failure(s, nsid, "boom").await.unwrap();
    let failed = store::get_publication(s, nsid).await.unwrap().unwrap();
    assert!(failed.withdrawn_at.is_some());
    assert_eq!(failed.last_error.as_deref(), Some("boom"));

    store::record_success(s, nsid, "did:plc:a", "at://did:plc:a/x/y", "bafycid")
        .await
        .unwrap();
    assert_eq!(
        store::get_publication(s, nsid)
            .await
            .unwrap()
            .unwrap()
            .withdrawn_at,
        None
    );

    store::record_withdrawn(s, "com.example.neverpublished")
        .await
        .unwrap();
    assert!(
        store::get_publication(s, "com.example.neverpublished")
            .await
            .unwrap()
            .unwrap()
            .withdrawn_at
            .is_some()
    );
}

#[tokio::test]
#[serial]
async fn a_rule_naming_a_missing_linked_repo_is_a_bad_request() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;

    let err = store::insert_rule(
        s,
        &pat("com.example.*"),
        "linked_repo",
        Some("no-such-grant"),
        false,
        "did:plc:a",
    )
    .await
    .unwrap_err();
    match err {
        AppError::BadRequest(msg) => assert_eq!(msg, "unknown linked repo"),
        other => panic!("expected bad request, got {other:?}"),
    }

    let rule = store::insert_rule(s, &pat("*"), "instance", None, false, "did:plc:a")
        .await
        .unwrap();
    let err = store::update_rule(
        s,
        &rule.id,
        Some("linked_repo"),
        Some(Some("no-such-grant")),
        None,
    )
    .await
    .unwrap_err();
    match err {
        AppError::BadRequest(msg) => assert_eq!(msg, "unknown linked repo"),
        other => panic!("expected bad request, got {other:?}"),
    }
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
    let pending = linked_repo_target(&grant(None, STATUS_PENDING, FULL_SCOPE), None);
    assert!(!pending.available);
    assert_eq!(
        pending.unavailable_reason.as_deref(),
        Some("this linked repo has not been authorized yet")
    );

    let reauth = linked_repo_target(
        &grant(Some("did:plc:a"), STATUS_NEEDS_REAUTH, FULL_SCOPE),
        None,
    );
    assert!(!reauth.available && !reauth.can_unpublish);
    assert_eq!(
        reauth.unavailable_reason.as_deref(),
        Some("this linked repo needs to be reauthorized")
    );

    let create_only = linked_repo_target(
        &grant(
            Some("did:plc:a"),
            STATUS_ACTIVE,
            "atproto repo:com.atproto.lexicon.schema?action=create",
        ),
        None,
    );
    assert!(!create_only.available);
    assert!(
        create_only
            .unavailable_reason
            .unwrap()
            .contains("doesn't have permission to write lexicon records")
    );

    let no_delete = linked_repo_target(
        &grant(
            Some("did:plc:a"),
            STATUS_ACTIVE,
            "atproto repo:com.atproto.lexicon.schema?action=create&action=update",
        ),
        None,
    );
    assert!(no_delete.available && !no_delete.can_unpublish);

    let full = linked_repo_target(&grant(Some("did:plc:a"), STATUS_ACTIVE, FULL_SCOPE), None);
    assert!(full.available && full.can_unpublish);
    assert_eq!(full.unavailable_reason, None);
    assert_eq!(full.kind, "linked_repo");
    assert_eq!(full.linked_repo_id.as_deref(), Some("g1"));
    assert_eq!(full.did.as_deref(), Some("did:plc:a"));
    assert_eq!(full.handle.as_deref(), Some("a.test"));
}

#[test]
fn a_granted_scope_narrower_than_the_request_decides() {
    let active = grant(Some("did:plc:a"), STATUS_ACTIVE, FULL_SCOPE);

    let reduced = linked_repo_target(&active, Some("atproto"));
    assert!(!reduced.available && !reduced.can_unpublish);
    assert_eq!(
        reduced.unavailable_reason.as_deref(),
        Some(GRANTED_FEWER_ATPROTO)
    );

    let no_delete = linked_repo_target(
        &active,
        Some("atproto repo:com.atproto.lexicon.schema?action=create&action=update"),
    );
    assert!(no_delete.available && !no_delete.can_unpublish);

    // A request that never asked for the collection keeps its own reason,
    // whatever the PDS granted.
    let unasked = linked_repo_target(
        &grant(Some("did:plc:a"), STATUS_ACTIVE, "atproto"),
        Some(FULL_SCOPE),
    );
    assert!(!unasked.available);
    assert!(
        unasked
            .unavailable_reason
            .unwrap()
            .contains("link it again")
    );
}

const GRANTED_FEWER_ATPROTO: &str =
    "this linked repo's PDS granted less access than requested. Try linking it again later";

async fn seed_linked_session(s: &happyview::AppState, did: &str, scope: Option<&str>) {
    let session = serde_json::json!({
        "dpop_key": {},
        "token_set": { "sub": did, "scope": scope, "access_token": "tok", "token_type": "DPoP" },
    });
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_linked_repo_sessions (did, session_data, updated_at) VALUES (?, ?, ?)",
        s.db_backend,
    );
    happyview::db::query(&sql)
        .bind(did)
        .bind(session.to_string())
        .bind(happyview::db::now_rfc3339())
        .execute(&s.db)
        .await
        .unwrap();
}

#[tokio::test]
#[serial]
async fn linked_repo_targets_judge_by_the_scope_in_the_stored_session() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;

    let reduced = active_grant(s, "did:plc:reduced", FULL_SCOPE).await;
    seed_linked_session(s, "did:plc:reduced", Some("atproto")).await;
    let full = active_grant(s, "did:plc:full", FULL_SCOPE).await;
    seed_linked_session(s, "did:plc:full", Some(FULL_SCOPE)).await;
    let unknown = active_grant(s, "did:plc:unknown", FULL_SCOPE).await;
    seed_linked_session(s, "did:plc:unknown", None).await;
    let missing = active_grant(s, "did:plc:missing", FULL_SCOPE).await;

    let linked_repos = LinkedRepoTargets::load(s).await.unwrap();
    let target = |id: &str| linked_repos.target(linked_repos.find(id).unwrap());

    let reduced_id = reduced.id.clone();
    let reduced = target(&reduced_id);
    assert!(!reduced.available && !reduced.can_unpublish);
    assert_eq!(
        reduced.unavailable_reason.as_deref(),
        Some(GRANTED_FEWER_ATPROTO)
    );
    for id in [&full.id, &unknown.id, &missing.id] {
        let t = target(id);
        assert!(t.available && t.can_unpublish, "{t:?}");
    }

    let listed = list_targets(s).await.unwrap();
    let listed_reduced = listed
        .iter()
        .find(|t| t.did.as_deref() == Some("did:plc:reduced"))
        .unwrap();
    assert_eq!(
        listed_reduced.unavailable_reason.as_deref(),
        Some(GRANTED_FEWER_ATPROTO)
    );

    // A rule on the reduced repo is refused with the same reason.
    let (status, body) = call(
        &app,
        "POST",
        &format!("{BASE}/rules"),
        Some(json!({
            "pattern": "com.example.*",
            "target_kind": "linked_repo",
            "linked_repo_id": reduced_id,
            "auto_publish": false,
        })),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body.to_string().contains("granted less access"), "{body}");
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
        Some("no publishing rule covers com.example.norule")
    );
    let row = store::get_publication(s, "com.example.norule")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.last_error.as_deref(),
        Some("no publishing rule covers com.example.norule")
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
    let linked_repos = LinkedRepoTargets::load(s).await.unwrap();
    let instance = instance_target(s).await.unwrap();
    let target = target_for_rule(s, &rule, &linked_repos, &instance).await;
    assert!(!target.available);
    assert_eq!(target.kind, "linked_repo");
    assert_eq!(target.did, None);
    assert_eq!(
        target.unavailable_reason.as_deref(),
        Some("the linked repo this rule used was removed")
    );

    let outcome = publish(s, nsid, None).await;
    assert_eq!(
        outcome.error.as_deref(),
        Some("the linked repo this rule used was removed")
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

#[tokio::test]
#[serial]
async fn reconcile_leaves_a_withdrawn_lexicon_alone_and_the_overview_says_so() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let nsid = "com.example.pulled";
    put_manual(s, nsid).await;
    // Writing through this grant would fail and mark it needs_reauth, so its
    // status staying active shows nothing was attempted.
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
    store::record_withdrawn(s, nsid).await.unwrap();
    let before = store::get_publication(s, nsid).await.unwrap().unwrap();

    reconcile(s.clone()).await;

    let after = store::get_publication(s, nsid).await.unwrap().unwrap();
    assert_eq!(after.last_error, None);
    assert_eq!(after.last_attempt_at, before.last_attempt_at);
    assert_eq!(count_events(s, "lexicon.publish_failed", nsid).await, 0);
    assert_eq!(
        linked::get(s, &repo.id).await.unwrap().unwrap().status,
        STATUS_ACTIVE
    );

    let (status, body) = call(&app, "GET", BASE, None).await;
    assert_eq!(status, 200, "{body}");
    let lexicon = &body["lexicons"][0];
    assert_eq!(lexicon["nsid"], nsid);
    assert_eq!(lexicon["status"], "withdrawn");
    assert!(lexicon["publication"]["withdrawn_at"].is_string(), "{body}");

    // A publish by hand is still an attempt: it fails here for want of a
    // session, and the lexicon stays withdrawn until one succeeds.
    let outcome = publish(s, nsid, Some("did:plc:admin")).await;
    assert!(!outcome.ok);
    let row = store::get_publication(s, nsid).await.unwrap().unwrap();
    assert!(row.last_error.is_some());
    assert!(row.withdrawn_at.is_some());
}

#[tokio::test]
#[serial]
async fn auto_publish_skips_a_lexicon_withdrawn_after_reconcile_chose_it() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let nsid = "com.example.raced";
    put_manual(s, nsid).await;
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
    store::record_withdrawn(s, nsid).await.unwrap();
    let before = store::get_publication(s, nsid).await.unwrap().unwrap();

    // What reconcile does for an NSID it chose before the unpublish landed.
    let outcome = publish(s, nsid, None).await;

    assert!(!outcome.ok);
    let after = store::get_publication(s, nsid).await.unwrap().unwrap();
    assert!(after.withdrawn_at.is_some());
    assert_eq!(after.last_error, None);
    assert_eq!(after.last_attempt_at, before.last_attempt_at);
    assert_eq!(count_events(s, "lexicon.publish_failed", nsid).await, 0);
    assert_eq!(
        linked::get(s, &repo.id).await.unwrap().unwrap().status,
        STATUS_ACTIVE
    );
}

#[tokio::test]
#[serial]
async fn unpublish_without_a_publication_is_not_found() {
    common::require_db!();
    let app = TestApp::new().await;

    let err = unpublish(&app.state, "com.example.never", "did:plc:admin")
        .await
        .unwrap_err();
    match err {
        AppError::NotFound(msg) => assert_eq!(msg, "com.example.never is not published"),
        other => panic!("expected not found, got {other:?}"),
    }
}

#[tokio::test]
#[serial]
async fn unpublish_from_a_did_nothing_holds_is_a_conflict_and_keeps_the_row() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let nsid = "com.example.stranded";
    store::record_success(
        s,
        nsid,
        "did:plc:gone",
        "at://did:plc:gone/com.atproto.lexicon.schema/com.example.stranded",
        "bafycid",
    )
    .await
    .unwrap();
    active_grant(s, "did:plc:someoneelse", FULL_SCOPE).await;

    let err = unpublish(s, nsid, "did:plc:admin").await.unwrap_err();
    match err {
        AppError::Conflict(msg) => assert_eq!(
            msg,
            "no linked repo or attached account holds did:plc:gone any more; delete the record \
             from that account directly"
        ),
        other => panic!("expected conflict, got {other:?}"),
    }
    assert!(store::get_publication(s, nsid).await.unwrap().is_some());
}

// ---------------------------------------------------------------------------
// Admin API
// ---------------------------------------------------------------------------

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call_as(
    app: &TestApp,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(cookie.0, cookie.1);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&b).unwrap())
        }
        None => Body::empty(),
    };
    let resp = app
        .router
        .clone()
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

async fn call(app: &TestApp, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    call_as(app, app.admin_cookie(), method, path, body).await
}

const BASE: &str = "/admin/lexicon-publishing";

#[tokio::test]
#[serial]
async fn overview_lists_a_manual_lexicon_with_no_rule_and_omits_network_ones() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    put_manual(s, "com.example.mine").await;
    s.lexicons
        .put_network(
            &s.db,
            s.db_backend,
            happyview::catalog::LexiconWrite {
                lexicon_json: record_lexicon("com.other.theirs"),
                backfill: false,
                target_collection: None,
                action: happyview::lexicon::ProcedureAction::Upsert,
                token_cost: None,
                authority_did: Some("did:plc:other".into()),
            },
        )
        .await
        .unwrap();

    let (status, body) = call(&app, "GET", BASE, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["rules"], json!([]));
    assert_eq!(body["targets"][0]["kind"], "instance");
    assert_eq!(body["targets"][0]["available"], false);

    let lexicons = body["lexicons"].as_array().unwrap();
    assert_eq!(lexicons.len(), 1, "{body}");
    assert_eq!(
        lexicons[0],
        json!({
            "nsid": "com.example.mine",
            "layer": "manual",
            "rule_id": null,
            "rule_pattern": null,
            "auto_publish": false,
            "target": null,
            "status": "no_rule",
            "publication": null,
        })
    );
}

#[tokio::test]
#[serial]
async fn an_instance_rule_on_did_web_is_refused_with_the_reason() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.setup_did_web().await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("{BASE}/rules"),
        Some(json!({"pattern": "*", "target_kind": "instance"})),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("did:web"),
        "{body}"
    );
    assert!(store::list_rules(&app.state).await.unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn an_existing_instance_rule_reports_its_target_unavailable_on_did_web() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.setup_did_web().await;
    put_manual(&app.state, "com.example.webbed").await;
    store::insert_rule(
        &app.state,
        &pat("*"),
        "instance",
        None,
        true,
        "did:plc:admin",
    )
    .await
    .unwrap();

    let (status, body) = call(&app, "GET", BASE, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["rules"][0]["target"]["available"], false);
    let lexicon = &body["lexicons"][0];
    assert_eq!(lexicon["status"], "target_unavailable");
    assert!(
        lexicon["target"]["unavailable_reason"]
            .as_str()
            .unwrap()
            .contains("did:web")
    );
}

#[tokio::test]
#[serial]
async fn rule_lifecycle_through_the_router() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    put_manual(s, "com.example.ruled").await;
    let repo = active_grant(s, "did:plc:repoa", FULL_SCOPE).await;
    let rules = format!("{BASE}/rules");

    let (status, created) = call(
        &app,
        "POST",
        &rules,
        Some(json!({"pattern": " com.example.* ", "target_kind": "linked_repo", "linked_repo_id": repo.id})),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["pattern"], "com.example.*");
    assert_eq!(created["auto_publish"], false);
    assert_eq!(created["linked_repo_id"], repo.id);
    assert_eq!(created["created_by"], app.admin_did);
    assert_eq!(created["target"]["did"], "did:plc:repoa");
    assert_eq!(created["target"]["available"], true);
    let id = created["id"].as_str().unwrap().to_string();

    let (_, overview) = call(&app, "GET", BASE, None).await;
    let lexicon = &overview["lexicons"][0];
    assert_eq!(lexicon["status"], "unpublished");
    assert_eq!(lexicon["rule_id"], id);
    assert_eq!(lexicon["rule_pattern"], "com.example.*");
    assert_eq!(lexicon["target"]["did"], "did:plc:repoa");

    let (status, body) = call(
        &app,
        "POST",
        &rules,
        Some(json!({"pattern": "COM.EXAMPLE.*", "target_kind": "linked_repo", "linked_repo_id": repo.id})),
    )
    .await;
    assert_eq!(status, 409, "{body}");

    for bad in ["com.*.foo", "**", "com", ""] {
        let (status, body) = call(
            &app,
            "POST",
            &rules,
            Some(json!({"pattern": bad, "target_kind": "linked_repo", "linked_repo_id": repo.id})),
        )
        .await;
        assert_eq!(status, 400, "{bad}: {body}");
    }
    let (status, body) = call(
        &app,
        "POST",
        &rules,
        Some(json!({"pattern": "net.example.*", "target_kind": "linked_repo", "linked_repo_id": "no-such-grant"})),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "unknown linked repo");
    let (status, _) = call(
        &app,
        "POST",
        &rules,
        Some(json!({"pattern": "net.example.*", "target_kind": "somewhere"})),
    )
    .await;
    assert_eq!(status, 400);

    let (status, patched) = call(
        &app,
        "PATCH",
        &format!("{rules}/{id}"),
        Some(json!({"auto_publish": true})),
    )
    .await;
    assert_eq!(status, 200, "{patched}");
    assert_eq!(patched["auto_publish"], true);
    assert_eq!(patched["linked_repo_id"], repo.id);

    let (status, body) = call(
        &app,
        "PATCH",
        &format!("{rules}/{id}"),
        Some(json!({"linked_repo_id": "no-such-grant"})),
    )
    .await;
    assert_eq!(status, 400, "{body}");

    let (status, _) = call(&app, "DELETE", &format!("{rules}/{id}"), None).await;
    assert_eq!(status, 204);
    let (status, _) = call(&app, "DELETE", &format!("{rules}/{id}"), None).await;
    assert_eq!(status, 404);
    let (status, _) = call(
        &app,
        "PATCH",
        &format!("{rules}/{id}"),
        Some(json!({"auto_publish": false})),
    )
    .await;
    assert_eq!(status, 404);

    for event in [
        "lexicon_publishing.rule_created",
        "lexicon_publishing.rule_updated",
        "lexicon_publishing.rule_deleted",
    ] {
        assert!(
            count_events(s, event, "com.example.*").await >= 1,
            "{event}"
        );
    }
}

#[tokio::test]
#[serial]
async fn publish_validates_the_batch_and_reports_each_failure() {
    common::require_db!();
    let app = TestApp::new().await;
    put_manual(&app.state, "com.example.batch").await;
    let publish = format!("{BASE}/publish");

    let (status, _) = call(&app, "POST", &publish, Some(json!({"nsids": []}))).await;
    assert_eq!(status, 400);
    let too_many: Vec<String> = (0..501).map(|i| format!("com.example.n{i}")).collect();
    let (status, _) = call(&app, "POST", &publish, Some(json!({"nsids": too_many}))).await;
    assert_eq!(status, 400);

    let (status, body) = call(
        &app,
        "POST",
        &publish,
        Some(json!({"nsids": ["com.example.batch"]})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        json!({"results": [{
            "nsid": "com.example.batch",
            "ok": false,
            "uri": null,
            "cid": null,
            "error": "no publishing rule covers com.example.batch",
        }]})
    );

    let (status, body) = call(
        &app,
        "POST",
        &format!("{BASE}/unpublish"),
        Some(json!({"nsid": "com.example.batch"})),
    )
    .await;
    assert_eq!(status, 404, "{body}");
}

/// A user holding exactly `permissions`, and a cookie that signs in as them.
async fn limited_user(
    s: &happyview::AppState,
    did: &str,
    permissions: &[&str],
) -> impl Fn() -> (axum::http::HeaderName, axum::http::HeaderValue) {
    let user_id = uuid::Uuid::new_v4().to_string();
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, 0, ?)",
        s.db_backend,
    );
    happyview::db::query(&sql)
        .bind(&user_id)
        .bind(did)
        .bind(&now)
        .execute(&s.db)
        .await
        .unwrap();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_user_permissions (user_id, permission, granted_at) VALUES (?, ?, ?)",
        s.db_backend,
    );
    for permission in permissions {
        happyview::db::query(&sql)
            .bind(&user_id)
            .bind(*permission)
            .bind(&now)
            .execute(&s.db)
            .await
            .unwrap();
    }
    let did = did.to_string();
    let key = s.cookie_key.clone();
    move || common::auth::admin_cookie_header(&did, &key)
}

#[tokio::test]
#[serial]
async fn writes_need_lexicons_publish() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let cookie = limited_user(s, "did:plc:reader", &["lexicons:read"]).await;
    let rule = store::insert_rule(s, &pat("*"), "instance", None, false, "did:plc:admin")
        .await
        .unwrap();
    let rule_path = format!("{BASE}/rules/{}", rule.id);

    let (status, _) = call_as(&app, cookie(), "GET", BASE, None).await;
    assert_eq!(status, 200);
    let (status, _) = call_as(
        &app,
        cookie(),
        "PATCH",
        &rule_path,
        Some(json!({"auto_publish": true})),
    )
    .await;
    assert_eq!(status, 403);
    let (status, _) = call_as(&app, cookie(), "DELETE", &rule_path, None).await;
    assert_eq!(status, 403);
    assert!(
        !store::get_rule(s, &rule.id)
            .await
            .unwrap()
            .unwrap()
            .auto_publish
    );
    let (status, _) = call_as(
        &app,
        cookie(),
        "POST",
        &format!("{BASE}/rules"),
        Some(json!({"pattern": "*", "target_kind": "instance"})),
    )
    .await;
    assert_eq!(status, 403);
    let (status, _) = call_as(
        &app,
        cookie(),
        "POST",
        &format!("{BASE}/publish"),
        Some(json!({"nsids": ["com.example.x"]})),
    )
    .await;
    assert_eq!(status, 403);
    let (status, _) = call_as(
        &app,
        cookie(),
        "POST",
        &format!("{BASE}/unpublish"),
        Some(json!({"nsid": "com.example.x"})),
    )
    .await;
    assert_eq!(status, 403);
}

#[tokio::test]
#[serial]
async fn dns_reports_one_entry_per_authority() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    put_manual(s, "test.lexpub.one").await;
    put_manual(s, "test.lexpub.two").await;
    put_manual(s, "test.lexpubother.three").await;

    let (status, body) = call(&app, "GET", &format!("{BASE}/dns"), None).await;
    assert_eq!(status, 200, "{body}");
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 2, "{body}");
    assert_eq!(entries[0]["authority"], "lexpub.test");
    assert_eq!(entries[0]["record_name"], "_lexicon.lexpub.test");
    assert_eq!(entries[1]["authority"], "lexpubother.test");
    for entry in entries {
        assert_eq!(entry["did"], Value::Null, "{entry}");
        assert!(
            entry["error"].is_null() || entry["error"].is_string(),
            "{entry}"
        );
    }
}

#[tokio::test]
#[serial]
async fn publishing_is_usable_only_once_some_account_can_take_a_publish() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;

    let (status, body) = call(&app, "GET", BASE, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["usable"], false, "{body}");

    let did = "did:plc:usablerepo";
    active_grant(s, did, FULL_SCOPE).await;
    seed_linked_session(s, did, Some(FULL_SCOPE)).await;

    let (_, body) = call(&app, "GET", BASE, None).await;
    assert_eq!(body["usable"], true, "{body}");
}

#[tokio::test]
#[serial]
async fn a_lexicon_dns_check_names_the_did_the_record_should_hold() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let did = "did:plc:dnsrepo";
    let grant = active_grant(s, did, FULL_SCOPE).await;
    seed_linked_session(s, did, Some(FULL_SCOPE)).await;
    let nsid = "test.lexdns.thing";
    put_manual(s, nsid).await;
    put_manual(s, "test.lexdnsnorule.thing").await;
    store::insert_rule(
        s,
        &pat("test.lexdns.*"),
        "linked_repo",
        Some(&grant.id),
        false,
        "did:plc:admin",
    )
    .await
    .unwrap();

    let (status, body) = call(&app, "GET", &format!("{BASE}/dns/{nsid}"), None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["nsid"], nsid);
    assert_eq!(body["authority"], "lexdns.test");
    assert_eq!(body["record_name"], "_lexicon.lexdns.test");
    assert_eq!(body["expected_did"], did);
    assert_eq!(body["did"], Value::Null, "{body}");

    // Nothing decides where these go, so there is nothing to tell the
    // operator to set.
    for unknown in ["test.lexdnsnorule.thing", "test.lexdns.notalexicon"] {
        let (status, body) = call(&app, "GET", &format!("{BASE}/dns/{unknown}"), None).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["expected_did"], Value::Null, "{unknown}: {body}");
    }

    let (status, _) = call(&app, "GET", &format!("{BASE}/dns/not-an-nsid"), None).await;
    assert_eq!(status, 400);

    // Without linked-repos:read the default's account stays hidden, and only
    // a publication, whose uri names the DID anyway, says where it is.
    let reader = limited_user(s, "did:plc:dnsreader", &["lexicons:read"]).await;
    let path = format!("{BASE}/dns/{nsid}");
    let (status, body) = call_as(&app, reader(), "GET", &path, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["expected_did"], Value::Null, "{body}");
    let uri = format!("at://{did}/com.atproto.lexicon.schema/{nsid}");
    store::record_success(s, nsid, did, &uri, "bafycid")
        .await
        .unwrap();
    let (_, body) = call_as(&app, reader(), "GET", &path, None).await;
    assert_eq!(body["expected_did"], did, "{body}");
}

// ---------------------------------------------------------------------------
// The instance's attached account
// ---------------------------------------------------------------------------

const ATTACHED: &str = "did:plc:attached";

async fn attach_account(s: &happyview::AppState) {
    happyview::service_identity::upsert_identity(
        &s.db,
        s.db_backend,
        &happyview::service_identity::IdentityMode::AttachAccount,
        Some(ATTACHED),
        None,
        None,
        Some(ATTACHED),
    )
    .await
    .unwrap();
}

/// Replaces the attached account's stored session with one granting `scope`.
async fn seed_attached_session(s: &happyview::AppState, scope: &str) {
    let sql = happyview::db::adapt_sql(
        "DELETE FROM happyview_oauth_sessions WHERE did = ?",
        s.db_backend,
    );
    happyview::db::query(&sql)
        .bind(ATTACHED)
        .execute(&s.db)
        .await
        .unwrap();
    let session = json!({
        "dpop_key": {},
        "token_set": { "sub": ATTACHED, "scope": scope, "access_token": "tok", "token_type": "DPoP" },
    });
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_oauth_sessions (did, session_data) VALUES (?, ?)",
        s.db_backend,
    );
    happyview::db::query(&sql)
        .bind(ATTACHED)
        .bind(session.to_string())
        .execute(&s.db)
        .await
        .unwrap();
}

#[tokio::test]
#[serial]
async fn the_instance_target_is_judged_by_the_scope_the_attached_session_granted() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    attach_account(s).await;

    let none = instance_target(s).await.unwrap();
    assert!(!none.available && !none.can_unpublish);
    assert_eq!(
        none.unavailable_reason.as_deref(),
        Some("the attached account needs to sign in again from Service Identity")
    );

    seed_attached_session(s, "atproto identity:*").await;
    let identity_only = instance_target(s).await.unwrap();
    assert!(!identity_only.available && !identity_only.can_unpublish);
    assert_eq!(
        identity_only.unavailable_reason.as_deref(),
        Some(
            "the attached account hasn't granted permission to write lexicon records. \
             Re-authenticate it from Service Identity"
        )
    );

    seed_attached_session(s, "").await;
    let empty = instance_target(s).await.unwrap();
    assert!(
        empty
            .unavailable_reason
            .unwrap()
            .contains("hasn't granted permission to write lexicon records")
    );

    seed_attached_session(
        s,
        "atproto identity:* repo:com.atproto.lexicon.schema?action=create&action=update",
    )
    .await;
    let no_delete = instance_target(s).await.unwrap();
    assert!(
        no_delete.available && !no_delete.can_unpublish,
        "{no_delete:?}"
    );

    seed_attached_session(
        s,
        &format!(
            "atproto identity:* {}",
            happyview::lexicon_publishing::SCHEMA_WRITE_SCOPE
        ),
    )
    .await;
    let full = instance_target(s).await.unwrap();
    assert!(full.available && full.can_unpublish, "{full:?}");
    assert_eq!(full.did.as_deref(), Some(ATTACHED));
    assert_eq!(full.unavailable_reason, None);
}

#[tokio::test]
#[serial]
async fn an_instance_rule_needs_the_schema_scope_on_the_attached_session() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    attach_account(s).await;
    seed_attached_session(s, "atproto identity:*").await;

    let rule = json!({"pattern": "*", "target_kind": "instance"});
    let (status, body) = call(&app, "POST", &format!("{BASE}/rules"), Some(rule.clone())).await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body.to_string()
            .contains("Re-authenticate it from Service Identity"),
        "{body}"
    );

    seed_attached_session(
        s,
        &format!(
            "atproto identity:* {}",
            happyview::lexicon_publishing::SCHEMA_WRITE_SCOPE
        ),
    )
    .await;
    let (status, body) = call(&app, "POST", &format!("{BASE}/rules"), Some(rule)).await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["target"]["did"], ATTACHED);
}

// ---------------------------------------------------------------------------
// Linked repos behind linked-repos:read
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn choosing_a_linked_repo_for_a_default_needs_linked_repos_read() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let grant = active_grant(s, "did:plc:linkedtarget", FULL_SCOPE).await;
    seed_linked_session(s, "did:plc:linkedtarget", Some(FULL_SCOPE)).await;
    let cookie = limited_user(
        s,
        "did:plc:publisher",
        &["lexicons:read", "lexicons:publish"],
    )
    .await;

    let (status, body) = call_as(
        &app,
        cookie(),
        "POST",
        &format!("{BASE}/rules"),
        Some(json!({
            "pattern": "com.example.*",
            "target_kind": "linked_repo",
            "linked_repo_id": grant.id,
        })),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["message"], "Missing permission: linked-repos:read");
    assert!(store::list_rules(s).await.unwrap().is_empty());

    let instance_rule = store::insert_rule(s, &pat("*"), "instance", None, false, "did:plc:admin")
        .await
        .unwrap();
    let (status, body) = call_as(
        &app,
        cookie(),
        "PATCH",
        &format!("{BASE}/rules/{}", instance_rule.id),
        Some(json!({"target_kind": "linked_repo", "linked_repo_id": grant.id})),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["message"], "Missing permission: linked-repos:read");
    let unchanged = store::get_rule(s, &instance_rule.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.target_kind, "instance");

    // Auto-publish on a default that already points at a linked repo chooses
    // no account, and the answer does not name the one it points at.
    let linked_rule = store::insert_rule(
        s,
        &pat("com.example.*"),
        "linked_repo",
        Some(&grant.id),
        false,
        "did:plc:admin",
    )
    .await
    .unwrap();
    let (status, body) = call_as(
        &app,
        cookie(),
        "PATCH",
        &format!("{BASE}/rules/{}", linked_rule.id),
        Some(json!({"auto_publish": true})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["auto_publish"], true);
    assert_eq!(body["linked_repo_id"], Value::Null);
    assert_eq!(body["target"]["did"], Value::Null);
    assert_eq!(body["target"]["available"], true);
}

#[tokio::test]
#[serial]
async fn the_overview_hides_which_linked_repo_from_a_caller_without_linked_repos_read() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let did = "did:plc:hiddenrepo";
    let grant = active_grant(s, did, FULL_SCOPE).await;
    seed_linked_session(s, did, Some(FULL_SCOPE)).await;
    let nsid = "com.example.hidden.thing";
    put_manual(s, nsid).await;
    store::insert_rule(
        s,
        &pat("com.example.*"),
        "linked_repo",
        Some(&grant.id),
        false,
        "did:plc:admin",
    )
    .await
    .unwrap();
    let uri = format!("at://{did}/com.atproto.lexicon.schema/{nsid}");
    store::record_success(s, nsid, did, &uri, "bafycid")
        .await
        .unwrap();

    let reader = limited_user(s, "did:plc:lexreader", &["lexicons:read"]).await;
    let (status, body) = call_as(&app, reader(), "GET", BASE, None).await;
    assert_eq!(status, 200, "{body}");

    let linked_target = body["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["kind"] == "linked_repo")
        .unwrap();
    let rule = &body["rules"][0];
    let lexicon = body["lexicons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["nsid"] == nsid)
        .unwrap();
    for target in [linked_target, &rule["target"], &lexicon["target"]] {
        for field in ["linked_repo_id", "did", "handle", "unavailable_reason"] {
            assert_eq!(target[field], Value::Null, "{field} in {target}");
        }
        assert_eq!(target["kind"], "linked_repo");
        assert_eq!(target["available"], true);
        assert_eq!(target["can_unpublish"], true);
    }
    assert_eq!(rule["linked_repo_id"], Value::Null);
    // The record is public in that repo, so its row keeps saying where.
    assert_eq!(lexicon["publication"]["did"], did);
    assert!(!body.to_string().contains(&grant.id), "{body}");

    let (status, body) = call(&app, "GET", BASE, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["rules"][0]["target"]["did"], did);
    assert_eq!(body["rules"][0]["linked_repo_id"], grant.id.as_str());
}

// ---------------------------------------------------------------------------
// Orphaned publications and repeated failures
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn a_publication_whose_lexicon_is_gone_is_listed_and_can_be_unpublished() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let did = "did:plc:orphanholder";
    let grant = active_grant(s, did, FULL_SCOPE).await;

    let orphan = "com.example.deleted";
    let uri = format!("at://{did}/com.atproto.lexicon.schema/{orphan}");
    store::record_success(s, orphan, did, &uri, "bafycid")
        .await
        .unwrap();
    // Withdrawn and never-published rows hold nothing on a PDS.
    store::record_withdrawn(s, "com.example.withdrawn")
        .await
        .unwrap();
    store::record_failure(s, "com.example.failed", "boom")
        .await
        .unwrap();
    let present = "com.example.present";
    put_manual(s, present).await;
    store::record_success(
        s,
        present,
        did,
        &format!("at://{did}/com.atproto.lexicon.schema/{present}"),
        "bafycid",
    )
    .await
    .unwrap();

    let (status, body) = call(&app, "GET", BASE, None).await;
    assert_eq!(status, 200, "{body}");
    let orphans = body["orphans"].as_array().unwrap();
    assert_eq!(orphans.len(), 1, "{body}");
    assert_eq!(orphans[0]["nsid"], orphan);
    assert_eq!(orphans[0]["did"], did);
    assert_eq!(orphans[0]["uri"], uri.as_str());

    // Unpublish finds the repo from the row's DID. With no stored session the
    // grant's PDS cannot be reached, which is as far as a test can follow it.
    let err = unpublish(s, orphan, "did:plc:admin").await.unwrap_err();
    match err {
        AppError::Auth(msg) => assert!(msg.contains("needs reauthorization"), "{msg}"),
        other => panic!("expected the linked repo's session error, got {other:?}"),
    }
    assert_eq!(
        linked::get(s, &grant.id).await.unwrap().unwrap().status,
        STATUS_NEEDS_REAUTH
    );
    assert_eq!(
        store::get_publication(s, orphan)
            .await
            .unwrap()
            .unwrap()
            .did
            .as_deref(),
        Some(did)
    );
}

#[tokio::test]
#[serial]
async fn a_repeated_publish_failure_is_logged_once_and_a_new_one_again() {
    common::require_db!();
    let app = TestApp::new().await;
    let s = &app.state;
    let nsid = "com.example.repeat";
    put_manual(s, nsid).await;

    publish(s, nsid, None).await;
    let first = store::get_publication(s, nsid).await.unwrap().unwrap();
    publish(s, nsid, None).await;
    let second = store::get_publication(s, nsid).await.unwrap().unwrap();
    assert_eq!(count_events(s, "lexicon.publish_failed", nsid).await, 1);
    assert_eq!(second.last_error, first.last_error);
    let at = |p: &store::Publication| {
        chrono::DateTime::parse_from_rfc3339(p.last_attempt_at.as_deref().unwrap()).unwrap()
    };
    assert!(at(&second) > at(&first));

    // An instance default with no instance identity fails for another reason.
    store::insert_rule(s, &pat("*"), "instance", None, false, "did:plc:admin")
        .await
        .unwrap();
    let outcome = publish(s, nsid, None).await;
    assert_ne!(outcome.error, first.last_error);
    assert_eq!(count_events(s, "lexicon.publish_failed", nsid).await, 2);
}
