//! A user's space data moving from HappyView to their PDS, end to end.
//!
//! Every step runs against real servers: HappyView's own `/auth/login` and
//! `/auth/callback`, a real OAuth grant with DPoP from pds.js, the migration job
//! run by the job worker, then sync and write forwarding over the resulting
//! session.
//!
//! Ignored by default; it needs the pds.js stack and a test database:
//!
//!   docker compose -f docker-compose.pdsjs.yml up -d --wait
//!   TEST_DATABASE_URL=… cargo test --test spaces_migration_e2e -- --ignored --nocapture
//!   docker compose -f docker-compose.pdsjs.yml down -v
//!
//! pds.js is the PDS here for two reasons. Its OAuth issuer is the origin it is
//! reached on, so it works over plain HTTP on localhost; atproto-pds always
//! advertises an `https://` issuer. And its consent form accepts the account
//! password and returns the redirect as JSON, so approval needs no browser.
//!
//! The records are plain JSON, because pds.js derives different CIDs for
//! `$link`, `$bytes` and large integers. `spaces_pdsjs_interop` covers that.

mod common;
mod interop_support;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::spaces::commit::verify_commit;
use happyview::spaces::host_mode::HostMode;
use happyview::spaces::lthash::{LtHashState, record_element};
use happyview::spaces::native_client::parse_signed_commit;
use happyview::spaces::native_sync::{SyncOutcome, sync_repo};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const PDS: &str = "http://localhost:2588";
const PLC: &str = "http://localhost:2585";
/// Matches `PDS_PASSWORD` in docker-compose.pdsjs.yml.
const PASSWORD: &str = "password123";
const SPACE_TYPE: &str = "com.example.forum";
const COLLECTION: &str = "com.example.note";

struct PdsAccount {
    did: String,
    token: String,
}

async fn pds_post(token: Option<&str>, method: &str, body: Value) -> (u16, Value) {
    let mut req = reqwest::Client::new()
        .post(format!("{PDS}/xrpc/{method}"))
        .json(&body);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    let resp = req
        .send()
        .await
        .expect("request to atproto-pds failed; is it running?");
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn pds_get(token: &str, method: &str, params: &[(&str, &str)]) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .get(format!("{PDS}/xrpc/{method}"))
        .query(params)
        .bearer_auth(token)
        .send()
        .await
        .expect("request to atproto-pds failed");
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// The pds.js account, signed in with a password session for direct PDS calls.
async fn pds_account() -> PdsAccount {
    let did = interop_support::ensure_pdsjs_account(PDS, PLC, PASSWORD).await;
    let (status, body) = pds_post(
        None,
        "com.atproto.server.createSession",
        json!({ "identifier": did, "password": PASSWORD }),
    )
    .await;
    assert_eq!(status, 200, "createSession failed: {body}");
    PdsAccount {
        did,
        token: body["accessJwt"].as_str().expect("accessJwt").to_string(),
    }
}

/// Send a request through HappyView's router as `did`.
async fn as_user(
    app: &TestApp,
    did: &str,
    req: axum::http::request::Builder,
    body: Body,
) -> (StatusCode, Value) {
    let (name, value) = common::auth::admin_cookie_header(did, &app.state.cookie_key);
    let resp = app
        .router
        .clone()
        .oneshot(req.header(name, value).body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn hv_post(app: &TestApp, did: &str, method: &str, body: Value) -> (StatusCode, Value) {
    as_user(
        app,
        did,
        Request::builder()
            .method("POST")
            .uri(format!("/xrpc/{method}"))
            .header("content-type", "application/json"),
        Body::from(body.to_string()),
    )
    .await
}

async fn enable_flags(app: &TestApp) {
    for flag in [
        happyview::feature_flags::FeatureFlag::SPACES_ENABLED,
        happyview::feature_flags::FeatureFlag::SPACES_PDS_MIGRATION,
    ] {
        let sql = happyview::db::adapt_sql(
            "INSERT INTO happyview_instance_settings (key, value) VALUES (?, 'true')",
            app.state.db_backend,
        );
        happyview::db::query(&sql)
            .bind(flag)
            .execute(&app.state.db)
            .await
            .expect("enable flag");
    }
}

/// The signing key the author's DID document publishes.
async fn author_key(did: &str) -> happyview::spaces::commit::SpaceVerifyingKey {
    happyview::spaces::native_client::author_signing_key(&reqwest::Client::new(), PLC, did)
        .await
        .expect("author key resolves")
}

async fn repo_host_mode(app: &TestApp, space_id: &str, did: &str) -> HostMode {
    let mut conn = app.state.db.acquire().await.unwrap();
    happyview::spaces::db::get_or_create_repo_state(&mut conn, app.state.db_backend, space_id, did)
        .await
        .unwrap()
        .host_mode
}

/// Wait for the job worker to finish the account's migration job.
async fn wait_for_migration(app: &TestApp) -> (String, Value) {
    let sql = happyview::db::adapt_sql(
        "SELECT status, result FROM happyview_jobs WHERE job_type = ? ORDER BY created_at DESC",
        app.state.db_backend,
    );
    for _ in 0..120 {
        let row: Option<(String, Option<String>)> = happyview::db::query_as(&sql)
            .bind(happyview::jobs::native::migrate_space_repo::JOB_TYPE)
            .fetch_optional(&app.state.db)
            .await
            .unwrap();
        match row {
            Some((status, result)) if status == "completed" || status == "failed" => {
                let result = result
                    .and_then(|r| serde_json::from_str(&r).ok())
                    .unwrap_or(Value::Null);
                return (status, result);
            }
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    panic!("the migration job did not finish within 60 seconds");
}

fn query_param(url: &str, key: &str) -> String {
    let url = reqwest::Url::parse(url).expect("authorize URL parses");
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| panic!("authorize URL has no {key}: {url}"))
}

#[tokio::test]
#[ignore]
#[serial]
async fn signing_in_moves_a_users_space_to_their_pds_and_keeps_it_in_sync() {
    common::require_db!();
    // Server-side errors surface only in logs; `--nocapture` shows them.
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("happyview=debug,atrium_oauth=debug,warn")
        .try_init();

    let account = pds_account().await;
    let scope = format!("space:{SPACE_TYPE}?collection={COLLECTION}");

    let mut app = TestApp::new().await;
    app.point_at_real_identity(PLC, &["atproto", &scope]);
    enable_flags(&app).await;

    // --- The user's space and records start on HappyView -------------------

    let skey = format!("mig{}", &Uuid::new_v4().simple().to_string()[..10]);
    let (status, body) = hv_post(
        &app,
        &account.did,
        "com.atproto.simplespace.createSpace",
        json!({ "type": SPACE_TYPE, "skey": skey }),
    )
    .await;
    assert!(
        status.is_success(),
        "createSpace on HappyView failed ({status}): {body}"
    );
    let space_uri = body["uri"].as_str().expect("space uri").to_string();

    for text in ["first", "second"] {
        let (status, body) = hv_post(
            &app,
            &account.did,
            "com.atproto.space.createRecord",
            json!({
                "space": space_uri,
                "collection": COLLECTION,
                "record": { "$type": COLLECTION, "text": text },
            }),
        )
        .await;
        assert!(
            status.is_success(),
            "createRecord on HappyView failed ({status}): {body}"
        );
    }

    let space = happyview::spaces::db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        &account.did,
        SPACE_TYPE,
        &skey,
    )
    .await
    .unwrap()
    .expect("space exists on HappyView");
    assert_eq!(
        repo_host_mode(&app, &space.id, &account.did).await,
        HostMode::Polyfill
    );

    // --- Sign in through HappyView -----------------------------------------

    let (status, body) = as_user(
        &app,
        &account.did,
        Request::builder().uri(format!(
            "/auth/login?handle={}&scope={}",
            urlencoding::encode(&account.did),
            urlencoding::encode(&format!("atproto {scope}")),
        )),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login failed: {body}");
    let authorize_url = body["url"].as_str().expect("authorize url");

    let resp = reqwest::Client::new()
        .post(format!("{PDS}/oauth/authorize"))
        .header("accept", "application/json")
        .form(&[
            ("request_uri", query_param(authorize_url, "request_uri")),
            ("client_id", query_param(authorize_url, "client_id")),
            ("password", PASSWORD.to_string()),
            ("action", "approve".to_string()),
        ])
        .send()
        .await
        .expect("authorize request");
    let status = resp.status();
    let approval: Value = resp.json().await.unwrap_or(Value::Null);
    assert!(
        status.is_success(),
        "authorize failed ({status}): {approval}"
    );
    let redirect = approval["redirect"].as_str().expect("redirect");
    let grant = json!({
        "code": query_param(redirect, "code"),
        "state": query_param(redirect, "state"),
        "iss": query_param(redirect, "iss"),
    });

    let (status, body) = as_user(
        &app,
        &account.did,
        Request::builder().uri(format!(
            "/auth/callback?code={}&state={}&iss={}",
            urlencoding::encode(grant["code"].as_str().expect("code")),
            urlencoding::encode(grant["state"].as_str().expect("state")),
            urlencoding::encode(grant["iss"].as_str().expect("iss")),
        )),
        Body::empty(),
    )
    .await;
    assert!(
        status.is_redirection() || status.is_success(),
        "callback failed ({status}): {body}"
    );

    let granted =
        happyview::jobs::native::migrate_space_repo::granted_scope(&app.state, &account.did)
            .await
            .unwrap()
            .expect("the callback stores an OAuth session");
    assert!(
        granted.split_whitespace().any(|s| s.starts_with("space:")),
        "the PDS granted no space: scope: {granted}"
    );

    // --- The migration job moves the repo ---------------------------------

    let worker = tokio::spawn(happyview::jobs::worker::run_worker(app.state.clone()));
    let (status, result) = wait_for_migration(&app).await;
    worker.abort();
    assert_eq!(status, "completed", "migration job failed: {result}");
    assert_eq!(
        result["status"], "migrated",
        "migration did not move the repo: {result}"
    );
    assert_eq!(
        repo_host_mode(&app, &space.id, &account.did).await,
        HostMode::Native
    );

    let (status, listed) = pds_get(
        &account.token,
        "com.atproto.space.listRecords",
        &[
            ("space", &space_uri),
            ("repo", &account.did),
            ("collection", COLLECTION),
        ],
    )
    .await;
    assert_eq!(status, 200, "listRecords on the PDS failed: {listed}");
    assert_eq!(
        listed["records"].as_array().map(Vec::len),
        Some(2),
        "{listed}"
    );

    let (status, latest) = pds_get(
        &account.token,
        "com.atproto.space.getLatestCommit",
        &[("space", &space_uri), ("repo", &account.did)],
    )
    .await;
    assert_eq!(status, 200, "getLatestCommit on the PDS failed: {latest}");
    let commit = parse_signed_commit(&latest["commit"]).expect("commit parses");
    verify_commit(
        &commit,
        &space_uri,
        &account.did,
        &author_key(&account.did).await,
    )
    .expect("the PDS's commit verifies");

    let records = happyview::spaces::db::list_all_space_records(
        &app.state.db,
        app.state.db_backend,
        &space.id,
        &account.did,
    )
    .await
    .unwrap();
    let mut fold = LtHashState::new();
    for r in &records {
        fold.add(&record_element(&r.collection, &r.rkey, &r.cid));
    }
    assert_eq!(
        commit.hash,
        fold.hash(),
        "the PDS holds a different record set"
    );

    // --- A write on the PDS reaches HappyView's index ---------------------

    let (status, written) = pds_post(
        Some(&account.token),
        "com.atproto.space.createRecord",
        json!({
            "space": space_uri,
            "repo": account.did,
            "collection": COLLECTION,
            "record": { "$type": COLLECTION, "text": "written on the PDS" },
        }),
    )
    .await;
    assert!(
        status < 300,
        "createRecord on the PDS failed ({status}): {written}"
    );

    let outcome = sync_repo(&app.state, &space, &account.did)
        .await
        .expect("sync runs");
    assert!(
        matches!(outcome, SyncOutcome::Applied { .. }),
        "sync did not apply the PDS write: {outcome:?}"
    );
    let synced = happyview::spaces::db::get_space_record(
        &app.state.db,
        app.state.db_backend,
        written["uri"].as_str().expect("uri"),
    )
    .await
    .unwrap();
    assert!(
        synced.is_some(),
        "HappyView did not index the record written on the PDS"
    );

    // --- A write sent to HappyView lands on the PDS -----------------------

    let (status, forwarded) = hv_post(
        &app,
        &account.did,
        "com.atproto.space.createRecord",
        json!({
            "space": space_uri,
            "collection": COLLECTION,
            "record": { "$type": COLLECTION, "text": "sent to HappyView" },
        }),
    )
    .await;
    assert!(
        status.is_success(),
        "HappyView did not forward the write ({status}): {forwarded}"
    );

    let rkey = forwarded["uri"]
        .as_str()
        .and_then(|uri| uri.rsplit('/').next())
        .expect("forwarded record uri")
        .to_string();
    assert_eq!(
        pds_record_text(&account, &space_uri, &rkey)
            .await
            .as_deref(),
        Some("sent to HappyView"),
        "the forwarded create is not on the PDS"
    );

    let (status, body) = hv_post(
        &app,
        &account.did,
        "com.atproto.space.putRecord",
        json!({
            "space": space_uri,
            "collection": COLLECTION,
            "rkey": rkey,
            "record": { "$type": COLLECTION, "text": "updated through HappyView" },
        }),
    )
    .await;
    assert!(
        status.is_success(),
        "HappyView did not forward the update ({status}): {body}"
    );
    assert_eq!(
        pds_record_text(&account, &space_uri, &rkey)
            .await
            .as_deref(),
        Some("updated through HappyView"),
        "the forwarded update is not on the PDS"
    );

    let (status, body) = hv_post(
        &app,
        &account.did,
        "com.atproto.space.deleteRecord",
        json!({ "space": space_uri, "collection": COLLECTION, "rkey": rkey }),
    )
    .await;
    assert!(
        status.is_success(),
        "HappyView did not forward the delete ({status}): {body}"
    );
    assert_eq!(
        pds_record_text(&account, &space_uri, &rkey).await,
        None,
        "the forwarded delete did not remove the record from the PDS"
    );
}

/// The `text` of a record on the PDS, or `None` when it is not there.
async fn pds_record_text(account: &PdsAccount, space_uri: &str, rkey: &str) -> Option<String> {
    let (status, listed) = pds_get(
        &account.token,
        "com.atproto.space.listRecords",
        &[
            ("space", space_uri),
            ("repo", &account.did),
            ("collection", COLLECTION),
        ],
    )
    .await;
    assert_eq!(status, 200, "listRecords on the PDS failed: {listed}");
    listed["records"]
        .as_array()
        .expect("records")
        .iter()
        .find(|r| r["rkey"] == rkey)
        .and_then(|r| r["value"]["text"].as_str())
        .map(str::to_string)
}
