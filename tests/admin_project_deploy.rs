//! `PUT` and `GET /admin/project`: a project deployed as a tar.gz, built by
//! the boot loader, stored, served, and served again at the next boot.
//!
//! One test walks a single app through a sequence of deploys, because each
//! step's outcome depends on what the previous one left deployed.
//!
//! Runs on in-memory SQLite, and on Postgres too when `TEST_DATABASE_URL`
//! names one.

mod common;

use axum::body::Body;
use axum::http::{Method, Request};
use flate2::Compression;
use flate2::write::GzEncoder;
use happyview::AppState;
use happyview::db::{DatabaseBackend, adapt_sql};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const ADMIN: (&str, &str) = ("user-admin", "did:plc:deployadmin");
const NOBODY: (&str, &str) = ("user-nobody", "did:plc:deploynobody");
/// The admin's key, limited to `project:deploy`.
const DEPLOY_KEY: &str = "hv_deploykey0123456789abcdef01234567";

const THING: &str = "com.example.thing";
const THING_PATH: &str = "src/lexicons/com/example/thing.json";
const OTHER: &str = "com.example.other";
const OTHER_PATH: &str = "src/lexicons/com/example/other.json";

#[derive(Clone, Copy)]
enum Caller {
    Cookie(&'static str),
    Key(&'static str),
}

struct App {
    state: AppState,
    router: axum::Router,
}

impl App {
    async fn send(
        &self,
        method: Method,
        uri: &str,
        caller: Caller,
        if_match: Option<&str>,
        body: Option<Vec<u8>>,
    ) -> (u16, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1");
        match caller {
            Caller::Cookie(did) => {
                let (name, value) = common::auth::admin_cookie_header(did, &self.state.cookie_key);
                request = request.header(name, value);
            }
            Caller::Key(key) => {
                request = request.header("authorization", format!("Bearer {key}"));
            }
        }
        if let Some(etag) = if_match {
            request = request.header("if-match", format!("\"{etag}\""));
        }
        let request = match body {
            Some(body) => request
                .header("content-type", "application/gzip")
                .body(Body::from(body))
                .unwrap(),
            None => request.body(Body::empty()).unwrap(),
        };
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("serve the request");
        let status = response.status().as_u16();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, body)
    }

    async fn deploy(&self, query: &str, if_match: Option<&str>, bundle: Vec<u8>) -> (u16, Value) {
        self.send(
            Method::PUT,
            &format!("/admin/project{query}"),
            Caller::Cookie(ADMIN.1),
            if_match,
            Some(bundle),
        )
        .await
    }

    async fn status(&self) -> (u16, Value) {
        self.send(
            Method::GET,
            "/admin/project",
            Caller::Cookie(ADMIN.1),
            None,
            None,
        )
        .await
    }

    fn served(&self) -> Vec<String> {
        self.state.project.load().lexicons.keys().cloned().collect()
    }

    async fn exec(&self, sql: &str, binds: &[&str]) {
        let sql = adapt_sql(sql, self.state.db_backend);
        let mut query = happyview::db::query(&sql);
        for bind in binds {
            query = query.bind(*bind);
        }
        query
            .execute(&self.state.db)
            .await
            .unwrap_or_else(|e| panic!("seed failed: {e}\n{sql}"));
    }
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn lexicon(id: &str) -> String {
    json!({
        "lexicon": 1,
        "id": id,
        "defs": {"main": {"type": "record", "key": "tid", "record": {"type": "object", "properties": {}}}}
    })
    .to_string()
}

fn bundle(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (path, contents) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, *contents).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

async fn app(
    pool: sqlx::AnyPool,
    backend: DatabaseBackend,
    project_root: Option<std::path::PathBuf>,
) -> App {
    let mut config = happyview::test_support::test_config(backend);
    config.public_url = "http://127.0.0.1:0".into();
    // Unroutable: nothing a deploy starts may reach the network.
    config.plc_url = "http://127.0.0.1:1".into();
    config.relay_url = "http://127.0.0.1:1".into();
    config.session_secret = "test-session-secret-0123456789abcdef".into();
    config.project_root = project_root;
    let state = happyview::state::state_with_pool_on(config, pool, backend);
    let router = happyview::server::router(state.clone());
    App { state, router }
}

async fn seed(app: &App) {
    app.exec("DELETE FROM happyview_project_deployments", &[])
        .await;
    for (id, _) in [ADMIN, NOBODY] {
        app.exec("DELETE FROM happyview_api_keys WHERE user_id = ?", &[id])
            .await;
        app.exec(
            "DELETE FROM happyview_user_permissions WHERE user_id = ?",
            &[id],
        )
        .await;
        app.exec("DELETE FROM happyview_users WHERE id = ?", &[id])
            .await;
    }
    let at = "2026-01-01T00:00:00+00:00";
    app.exec(
        "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, 1, ?)",
        &[ADMIN.0, ADMIN.1, at],
    )
    .await;
    app.exec(
        "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, 0, ?)",
        &[NOBODY.0, NOBODY.1, at],
    )
    .await;
    app.exec(
        "INSERT INTO happyview_api_keys (id, user_id, name, key_hash, key_prefix, permissions, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        &[
            "key-deploy",
            ADMIN.0,
            "deploy only",
            &sha256(DEPLOY_KEY.as_bytes()),
            "hv_deplo",
            r#"["project:deploy"]"#,
            at,
        ],
    )
    .await;
}

#[tokio::test]
async fn a_project_deploys_redeploys_and_survives_a_boot() {
    let _lock = common::db::acquire_test_lock().await;
    for (pool, backend) in happyview::test_support::test_pools().await {
        let label = format!("{backend:?}");
        happyview::db::migrate(&pool, backend).await;
        let app = app(pool.clone(), backend, None).await;
        seed(&app).await;
        let thing = lexicon(THING);
        let other = lexicon(OTHER);
        let v1 = bundle(&[(THING_PATH, thing.as_bytes())]);
        let v2 = bundle(&[(OTHER_PATH, other.as_bytes())]);

        // Nothing deployed yet.
        let (status, body) = app.status().await;
        assert_eq!(status, 404, "{label}: {body}");
        assert_eq!(body["error"], "NoProject", "{label}");

        // A dry run reports and applies nothing, backfills included.
        let with_backfill = bundle(&[
            (
                "happyview.toml",
                format!("[lexicons.\"{THING}\"]\nbackfill = true\n").as_bytes(),
            ),
            (THING_PATH, thing.as_bytes()),
        ]);
        let (status, body) = app.deploy("?dry_run=true", None, with_backfill).await;
        assert_eq!(status, 200, "{label}: {body}");
        assert_eq!(body["dry_run"], true, "{label}");
        assert_eq!(body["deployed_at"], Value::Null, "{label}");
        assert_eq!(
            body["changes"]["lexicons"]["added"],
            json!([THING]),
            "{label}"
        );
        assert_eq!(body["backfills"], json!([THING]), "{label}");
        assert_eq!(
            app.status().await.0,
            404,
            "{label}: a dry run stored the bundle"
        );
        assert!(
            app.served().is_empty(),
            "{label}: a dry run installed the project"
        );

        // The real thing.
        let (status, body) = app.deploy("", None, v1.clone()).await;
        assert_eq!(status, 200, "{label}: {body}");
        assert_eq!(body["sha256"], sha256(&v1), "{label}");
        assert_eq!(
            body["changes"]["lexicons"]["added"],
            json!([THING]),
            "{label}"
        );
        assert_eq!(body["backfills"], json!([]), "{label}");
        let deployed_at = body["deployed_at"]
            .as_str()
            .expect("deployed_at")
            .to_string();
        assert_eq!(app.served(), [THING], "{label}");
        let (status, body) = app.status().await;
        assert_eq!(status, 200, "{label}: {body}");
        assert_eq!(body["sha256"], sha256(&v1), "{label}");
        assert_eq!(body["deployed_at"], deployed_at, "{label}");
        assert_eq!(body["deployed_by"], ADMIN.1, "{label}");
        assert!(body.get("error").is_none(), "{label}: {body}");

        // The same bundle again changes nothing and stores nothing new.
        let (status, body) = app.deploy("", Some(&sha256(&v1)), v1.clone()).await;
        assert_eq!(status, 200, "{label}: {body}");
        for kind in [
            "lexicons",
            "scripts",
            "modules",
            "network_lexicons",
            "labelers",
        ] {
            assert_eq!(
                body["changes"][kind],
                json!({"added": [], "changed": [], "removed": []}),
                "{label}: {kind}"
            );
        }
        assert_eq!(body["deployed_at"], deployed_at, "{label}");

        // A deploy that did not see the current one is refused.
        let (status, body) = app
            .deploy("", Some("not-the-current-one"), v2.clone())
            .await;
        assert_eq!(status, 412, "{label}: {body}");
        assert_eq!(body["current"], sha256(&v1), "{label}");
        assert_eq!(app.served(), [THING], "{label}");

        // A file removed locally is removed on deploy.
        let (status, body) = app.deploy("", Some(&sha256(&v1)), v2.clone()).await;
        assert_eq!(status, 200, "{label}: {body}");
        assert_eq!(
            body["changes"]["lexicons"]["added"],
            json!([OTHER]),
            "{label}"
        );
        assert_eq!(
            body["changes"]["lexicons"]["removed"],
            json!([THING]),
            "{label}"
        );
        assert_eq!(app.served(), [OTHER], "{label}");

        // A project that does not build applies nothing.
        let broken = bundle(&[(THING_PATH, lexicon("com.example.wrong").as_bytes())]);
        let (status, body) = app.deploy("", None, broken).await;
        assert_eq!(status, 422, "{label}: {body}");
        assert_eq!(body["error"], "InvalidProject", "{label}");
        assert!(
            body["problems"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["path"] == THING_PATH),
            "{label}: {body}"
        );
        assert_eq!(app.served(), [OTHER], "{label}");
        assert_eq!(app.status().await.1["sha256"], sha256(&v2), "{label}");

        // Neither does an archive with something in it no project holds.
        let unsafe_bundle = bundle(&[(".env", b"SECRET=1"), (OTHER_PATH, other.as_bytes())]);
        let (status, body) = app.deploy("", None, unsafe_bundle).await;
        assert_eq!(status, 422, "{label}: {body}");
        assert_eq!(body["problems"][0]["path"], ".env", "{label}: {body}");

        // An oversized body is refused before it is read as an archive.
        let oversized = vec![0u8; 50 * 1024 * 1024 + 1];
        let (status, body) = app.deploy("?dry_run=true", None, oversized).await;
        assert_eq!(status, 413, "{label}: {body}");

        // The permission, from a cookie without it and a key with it.
        let (status, body) = app
            .send(
                Method::PUT,
                "/admin/project?dry_run=true",
                Caller::Cookie(NOBODY.1),
                None,
                Some(v2.clone()),
            )
            .await;
        assert_eq!(status, 403, "{label}: {body}");
        assert!(
            body["message"].as_str().unwrap().contains("project:deploy"),
            "{label}: {body}"
        );
        let (status, body) = app
            .send(
                Method::PUT,
                "/admin/project?dry_run=true",
                Caller::Key(DEPLOY_KEY),
                None,
                Some(v2.clone()),
            )
            .await;
        assert_eq!(status, 200, "{label}: {body}");
        let (status, _) = app
            .send(
                Method::GET,
                "/admin/project",
                Caller::Key(DEPLOY_KEY),
                None,
                None,
            )
            .await;
        assert_eq!(status, 200, "{label}");

        // A boot serves what was deployed.
        app.state.project.store(std::sync::Arc::new(
            happyview::project::ProjectSnapshot::empty(),
        ));
        assert!(
            happyview::project::deploy::serve_stored(&app.state)
                .await
                .is_some(),
            "{label}: the stored project was not served"
        );
        assert_eq!(app.served(), [OTHER], "{label}");

        // A stored project that no longer builds leaves the instance up,
        // serving none, and says why; a fixed deploy clears it.
        happyview::project::deploy::store::save(
            &app.state.db,
            app.state.db_backend,
            "corrupt",
            b"not an archive",
            ADMIN.1,
        )
        .await
        .unwrap();
        app.state.project.store(std::sync::Arc::new(
            happyview::project::ProjectSnapshot::empty(),
        ));
        assert!(
            happyview::project::deploy::serve_stored(&app.state)
                .await
                .is_none(),
            "{label}"
        );
        let (status, body) = app.status().await;
        assert_eq!(status, 200, "{label}: {body}");
        assert_eq!(body["sha256"], "corrupt", "{label}");
        assert!(
            !body["error"]["problems"].as_array().unwrap().is_empty(),
            "{label}: {body}"
        );
        let (status, body) = app.deploy("", None, v2.clone()).await;
        assert_eq!(status, 200, "{label}: {body}");
        assert!(app.status().await.1.get("error").is_none(), "{label}");

        // A network lexicon the deployed file declares can't be deleted from
        // the dashboard, which the next deploy would undo.
        let declaring = bundle(&[
            (
                "happyview.toml",
                b"[network_lexicons]\ninclude = [\"app.example.declared\"]\n",
            ),
            (OTHER_PATH, other.as_bytes()),
        ]);
        let (status, body) = app.deploy("", None, declaring).await;
        assert_eq!(status, 200, "{label}: {body}");
        assert_eq!(
            body["changes"]["network_lexicons"]["added"],
            json!(["app.example.declared"]),
            "{label}"
        );
        for path in [
            "/admin/network-lexicons/app.example.declared",
            "/admin/lexicons/app.example.declared",
        ] {
            let (status, body) = app
                .send(Method::DELETE, path, Caller::Cookie(ADMIN.1), None, None)
                .await;
            assert_eq!(status, 409, "{label}: {path}: {body}");
        }

        // An instance serving HAPPYVIEW_PROJECT takes no deploys.
        let root = tempfile::tempdir().unwrap();
        let from_file = self::app(pool, backend, Some(root.path().to_path_buf())).await;
        let (status, body) = from_file.deploy("", None, v1.clone()).await;
        assert_eq!(status, 409, "{label}: {body}");
        assert_eq!(body["error"], "ProjectFromFile", "{label}");
    }
}
