//! A `TOKEN_ENCRYPTION_KEY` rotation, store by store, on every backend the
//! test run has: a value sealed under the old key still opens once the new key
//! is current and the old one is `TOKEN_ENCRYPTION_KEY_PREVIOUS`, a value
//! written mid-rotation is sealed under the new key, and nothing opens under
//! the new key alone until it has been re-sealed. Re-sealing then leaves
//! nothing needing the previous key, and everything opens under the new key
//! alone.
//!
//! The full rotation re-seals every store of the shared Postgres, so every test
//! here holds the `encryption_rotation` key; the singleton key tables are also
//! cleared per run, which the delegation key's own tests hold `delegation_key`
//! for.

use atrium_api::types::string::Did;
use atrium_common::store::Store;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::auth::oauth_store::{DbSessionStore, DbStateStore};
use crate::db::{DatabaseBackend, now_rfc3339};
use crate::plugin::encryption::{Keyring, encrypt};
use crate::test_support::test_pools;

const OLD: [u8; 32] = [0x11; 32];
const NEW: [u8; 32] = [0x22; 32];

fn old() -> Keyring {
    Keyring::single(OLD)
}

fn rotating() -> Keyring {
    Keyring::new(NEW, Some(OLD))
}

fn new_only() -> Keyring {
    Keyring::single(NEW)
}

async fn api_client(pool: &sqlx::AnyPool, backend: DatabaseBackend, id: &str) {
    let now = now_rfc3339();
    crate::db::query(&crate::db::adapt_sql(
        "INSERT INTO happyview_api_clients (id, client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, scopes, client_type, is_active, created_by, created_at, updated_at) VALUES (?, ?, '', 'test', ?, '', '[]', '', 'confidential', 1, 'did:plc:owner', ?, ?)",
        backend,
    ))
    .bind(id)
    .bind(format!("key-{id}"))
    .bind(format!("https://{id}.example/client-metadata.json"))
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .unwrap();
}

async fn plugin_row(pool: &sqlx::AnyPool, backend: DatabaseBackend, id: &str) {
    crate::db::query(&crate::db::adapt_sql(
        "INSERT INTO happyview_plugins (id, source, api_version) VALUES (?, 'file', '2')",
        backend,
    ))
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
#[serial_test::serial(encryption_rotation)]
async fn dpop_keys_and_sessions_open_with_the_previous_key() {
    for (pool, backend) in test_pools().await {
        let client = format!("rot-{}", uuid::Uuid::new_v4());
        api_client(&pool, backend, &client).await;
        let keypair = crate::oauth::keys::generate_dpop_keypair().unwrap();
        let (key_id, provision) = (format!("{client}-k"), format!("{client}-p"));
        let did = format!("did:plc:{client}");
        crate::oauth::keys::store_dpop_key(
            &pool, backend, &OLD, &key_id, &provision, &client, &keypair, None,
        )
        .await
        .unwrap();
        crate::oauth::sessions::store_dpop_session(
            &pool,
            backend,
            &OLD,
            &format!("{client}-s"),
            &client,
            &key_id,
            &did,
            "access",
            Some("refresh"),
            None,
            "atproto",
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let (_, _, jwk, _, _) =
            crate::oauth::keys::get_dpop_key(&pool, backend, &rotating(), &provision)
                .await
                .unwrap();
        assert_eq!(jwk, keypair.private_jwk);
        let session = crate::oauth::sessions::get_dpop_session(
            &pool,
            backend,
            &rotating(),
            &client,
            &did,
            &key_id,
        )
        .await
        .unwrap();
        assert_eq!(session.access_token, "access");
        assert_eq!(session.refresh_token.as_deref(), Some("refresh"));

        assert!(
            crate::oauth::keys::get_dpop_key(&pool, backend, &new_only(), &provision)
                .await
                .is_err()
        );
        assert!(
            crate::oauth::sessions::get_dpop_session(
                &pool,
                backend,
                &new_only(),
                &client,
                &did,
                &key_id,
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
#[serial_test::serial(encryption_rotation)]
async fn oauth_client_keys_open_with_the_previous_key_and_new_ones_seal_with_the_new_key() {
    for (pool, backend) in test_pools().await {
        let owner = format!("rot-{}", uuid::Uuid::new_v4());
        let sealed_old = crate::oauth::client_keys::generate_client_key(&owner).unwrap();
        crate::oauth::client_keys::insert_key(&pool, backend, Some(&old()), &sealed_old)
            .await
            .unwrap();

        let loaded =
            crate::oauth::client_keys::load_keys(&pool, backend, Some(&rotating()), &owner)
                .await
                .unwrap();
        assert_eq!(loaded[0].private_jwk, sealed_old.private_jwk);
        assert!(
            crate::oauth::client_keys::load_keys(&pool, backend, Some(&new_only()), &owner)
                .await
                .is_err()
        );

        let other = format!("rot-{}", uuid::Uuid::new_v4());
        let sealed_new = crate::oauth::client_keys::generate_client_key(&other).unwrap();
        crate::oauth::client_keys::insert_key(&pool, backend, Some(&rotating()), &sealed_new)
            .await
            .unwrap();
        let loaded =
            crate::oauth::client_keys::load_keys(&pool, backend, Some(&new_only()), &other)
                .await
                .unwrap();
        assert_eq!(loaded[0].private_jwk, sealed_new.private_jwk);
    }
}

#[tokio::test]
#[serial_test::serial(encryption_rotation)]
async fn external_tokens_open_with_the_previous_key() {
    for (pool, backend) in test_pools().await {
        let did = format!("did:plc:rot-{}", uuid::Uuid::new_v4());
        let plugin = format!("rot{}", uuid::Uuid::new_v4().simple());
        plugin_row(&pool, backend, &plugin).await;
        crate::external_auth::tokens::store_tokens(
            &pool,
            backend,
            Some(&OLD),
            &did,
            &plugin,
            "acct",
            "access",
            Some("refresh"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let tokens = crate::external_auth::tokens::get_tokens(
            &pool,
            backend,
            Some(&rotating()),
            &did,
            &plugin,
        )
        .await
        .unwrap();
        assert_eq!(tokens.access_token, "access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh"));
        assert!(
            crate::external_auth::tokens::get_tokens(
                &pool,
                backend,
                Some(&new_only()),
                &did,
                &plugin
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
#[serial_test::serial(encryption_rotation)]
async fn plugin_secrets_open_with_the_previous_key() {
    for (pool, backend) in test_pools().await {
        let plugin = format!("rot{}", uuid::Uuid::new_v4().simple());
        let name = format!(
            "{}API_KEY",
            crate::plugin::secrets::secret_env_prefix(&plugin)
        );
        plugin_row(&pool, backend, &plugin).await;
        let sealed = STANDARD.encode(encrypt(&OLD, b"shh").unwrap());
        let config = serde_json::json!({ "secrets": { name: sealed } }).to_string();
        crate::db::query(&crate::db::adapt_sql(
            "INSERT INTO happyview_plugin_configs (plugin_id, config, updated_at) VALUES (?, ?, ?)",
            backend,
        ))
        .bind(&plugin)
        .bind(&config)
        .bind(now_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        let secrets =
            crate::plugin::secrets::load_plugin_secrets(&pool, backend, Some(&rotating()), &plugin)
                .await;
        assert_eq!(secrets.get("API_KEY").map(String::as_str), Some("shh"));
        let secrets =
            crate::plugin::secrets::load_plugin_secrets(&pool, backend, Some(&new_only()), &plugin)
                .await;
        assert!(!secrets.contains_key("API_KEY"));
    }
}

#[tokio::test]
#[serial_test::serial(encryption_rotation)]
async fn verification_method_keys_open_with_the_previous_key() {
    for (pool, backend) in test_pools().await {
        let fragment = format!("#rot{}", uuid::Uuid::new_v4().simple());
        crate::verification_methods::create_method(&pool, backend, &fragment, &OLD)
            .await
            .unwrap();
        let via_old =
            crate::verification_methods::get_private_key_bytes(&pool, backend, &fragment, &old())
                .await
                .unwrap();
        let via_rotation = crate::verification_methods::get_private_key_bytes(
            &pool,
            backend,
            &fragment,
            &rotating(),
        )
        .await
        .unwrap();
        assert!(via_old.is_some());
        assert_eq!(via_rotation, via_old);
        assert!(
            crate::verification_methods::get_private_key_bytes(
                &pool,
                backend,
                &fragment,
                &new_only(),
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
#[serial_test::serial(delegation_key, encryption_rotation)]
async fn the_delegation_and_attestation_keys_open_with_the_previous_key() {
    for (pool, backend) in test_pools().await {
        crate::db::query("DELETE FROM happyview_delegation_key")
            .execute(&pool)
            .await
            .unwrap();
        crate::db::query("DELETE FROM happyview_attestation_key")
            .execute(&pool)
            .await
            .unwrap();

        let delegation = crate::spaces::delegation_key::load_or_create(&pool, backend, &old())
            .await
            .unwrap();
        let reopened = crate::spaces::delegation_key::load(&pool, backend, &rotating())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reopened.to_bytes(), delegation.to_bytes());
        assert!(
            crate::spaces::delegation_key::load(&pool, backend, &new_only())
                .await
                .is_err()
        );

        let url = "https://rotation.example";
        let signer =
            crate::plugin::attestation::load_or_generate(&pool, backend, url, Some(&old()))
                .await
                .unwrap();
        let reopened =
            crate::plugin::attestation::load_or_generate(&pool, backend, url, Some(&rotating()))
                .await
                .unwrap();
        assert_eq!(reopened.public_key_bytes(), signer.public_key_bytes());
        assert!(
            crate::plugin::attestation::load_or_generate(&pool, backend, url, Some(&new_only()))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
#[serial_test::serial(encryption_rotation)]
async fn script_variables_open_with_the_previous_key_and_saves_seal_with_the_new_key() {
    for (pool, backend) in test_pools().await {
        crate::db::query("DELETE FROM happyview_script_variables")
            .execute(&pool)
            .await
            .unwrap();
        let sealed = crate::script::variables::seal(Some(&old()), "secret").unwrap();
        crate::db::query(&crate::db::adapt_sql(
            "INSERT INTO happyview_script_variables (key, value, encrypted, created_at) VALUES ('API_KEY', ?, ?, ?)",
            backend,
        ))
        .bind(&sealed.value)
        .bind(sealed.encrypted)
        .bind(now_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        let env = crate::script::variables::load(&pool, backend, Some(&rotating())).await;
        assert_eq!(env.get("API_KEY").map(String::as_str), Some("secret"));
        assert!(
            crate::script::variables::load(&pool, backend, Some(&new_only()))
                .await
                .is_empty()
        );

        let resealed = crate::script::variables::seal(Some(&rotating()), "secret").unwrap();
        assert_eq!(
            crate::script::variables::open(Some(&new_only()), &resealed.value, true).as_deref(),
            Some("secret")
        );
    }
}

#[tokio::test]
#[serial_test::serial(encryption_rotation)]
async fn oauth_sessions_and_state_open_with_the_previous_key() {
    for (pool, backend) in test_pools().await {
        let did = Did::new(format!("did:plc:rot{}", uuid::Uuid::new_v4().simple())).unwrap();
        for table in [
            crate::auth::oauth_store::SESSIONS_TABLE,
            crate::linked_repos::client::LINKED_SESSIONS_TABLE,
        ] {
            DbSessionStore::new_with_table(pool.clone(), backend, table, Some(old()))
                .set(
                    did.clone(),
                    crate::auth::oauth_store::tests::sample_session("access"),
                )
                .await
                .unwrap();
            let rotating_store =
                DbSessionStore::new_with_table(pool.clone(), backend, table, Some(rotating()));
            assert!(rotating_store.get(&did).await.unwrap().is_some(), "{table}");
            let new_store =
                DbSessionStore::new_with_table(pool.clone(), backend, table, Some(new_only()));
            assert!(new_store.get(&did).await.unwrap().is_none(), "{table}");
        }

        let state_key = format!("rot-{}", uuid::Uuid::new_v4());
        DbStateStore::new(pool.clone(), backend, Some(old()))
            .set(
                state_key.clone(),
                crate::auth::oauth_store::tests::sample_state(),
            )
            .await
            .unwrap();
        assert!(
            DbStateStore::new(pool.clone(), backend, Some(rotating()))
                .get(&state_key)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            DbStateStore::new(pool.clone(), backend, Some(new_only()))
                .get(&state_key)
                .await
                .unwrap()
                .is_none()
        );
    }
}

/// Every stored value opens through a `Keyring`, so a reader cannot quietly
/// try the current key alone: only `plugin::encryption` calls the raw
/// `decrypt`.
#[test]
fn only_the_encryption_module_calls_the_raw_decrypt() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs")
                || path.ends_with("plugin/encryption.rs")
                || path.ends_with("encryption_rotation_tests.rs")
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (n, line) in text.lines().enumerate() {
                let raw = line
                    .match_indices("decrypt(")
                    .any(|(i, _)| i == 0 || !line[..i].ends_with('.'));
                if raw && !line.trim_start().starts_with("//") {
                    offenders.push(format!(
                        "{}:{}",
                        path.strip_prefix(&root).unwrap().display(),
                        n + 1
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "open stored values with Keyring::decrypt: {offenders:?}"
    );
}

/// Something sealed under the old key in every store, keyed by what each
/// reader looks it up by.
struct Seeded {
    client: String,
    provision: String,
    key_id: String,
    did: String,
    owner_sealed: String,
    owner_plain: String,
    plugin: String,
    fragment: String,
    space_did_id: String,
    session_did: Did,
    state_key: String,
}

async fn seed_everything_under_the_old_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
) -> Seeded {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let seeded = Seeded {
        client: format!("rot-{id}"),
        provision: format!("rot-{id}-p"),
        key_id: format!("rot-{id}-k"),
        did: format!("did:plc:rot{id}"),
        owner_sealed: format!("rot-{id}-sealed"),
        owner_plain: format!("rot-{id}-plain"),
        plugin: format!("rot{id}"),
        fragment: format!("#rot{id}"),
        space_did_id: format!("rot-{id}-space"),
        session_did: Did::new(format!("did:plc:rot{id}")).unwrap(),
        state_key: format!("rot-{id}-state"),
    };

    api_client(pool, backend, &seeded.client).await;
    let keypair = crate::oauth::keys::generate_dpop_keypair().unwrap();
    crate::oauth::keys::store_dpop_key(
        pool,
        backend,
        &OLD,
        &seeded.key_id,
        &seeded.provision,
        &seeded.client,
        &keypair,
        None,
    )
    .await
    .unwrap();
    crate::oauth::sessions::store_dpop_session(
        pool,
        backend,
        &OLD,
        &format!("{}-s", seeded.client),
        &seeded.client,
        &seeded.key_id,
        &seeded.did,
        "access",
        Some("refresh"),
        None,
        "atproto",
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let sealed = crate::oauth::client_keys::generate_client_key(&seeded.owner_sealed).unwrap();
    crate::oauth::client_keys::insert_key(pool, backend, Some(&old()), &sealed)
        .await
        .unwrap();
    // Stored before the instance had a key, as an `encrypted = 0` row.
    let plain = crate::oauth::client_keys::generate_client_key(&seeded.owner_plain).unwrap();
    crate::oauth::client_keys::insert_key(pool, backend, None, &plain)
        .await
        .unwrap();

    plugin_row(pool, backend, &seeded.plugin).await;
    crate::external_auth::tokens::store_tokens(
        pool,
        backend,
        Some(&OLD),
        &seeded.did,
        &seeded.plugin,
        "acct",
        "access",
        Some("refresh"),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let name = format!(
        "{}API_KEY",
        crate::plugin::secrets::secret_env_prefix(&seeded.plugin)
    );
    let config =
        serde_json::json!({ "secrets": { name: STANDARD.encode(encrypt(&OLD, b"shh").unwrap()) } });
    crate::db::query(&crate::db::adapt_sql(
        "INSERT INTO happyview_plugin_configs (plugin_id, config, updated_at) VALUES (?, ?, ?)",
        backend,
    ))
    .bind(&seeded.plugin)
    .bind(config.to_string())
    .bind(now_rfc3339())
    .execute(pool)
    .await
    .unwrap();

    crate::db::query("DELETE FROM happyview_service_identity")
        .execute(pool)
        .await
        .unwrap();
    crate::db::query(&crate::db::adapt_sql(
        "INSERT INTO happyview_service_identity (id, mode, signing_key_enc, rotation_key_enc, created_at, updated_at) VALUES (1, 'not_exposed', ?, ?, ?, ?)",
        backend,
    ))
    .bind(STANDARD.encode(encrypt(&OLD, b"signing").unwrap()))
    .bind(STANDARD.encode(encrypt(&OLD, b"rotation").unwrap()))
    .bind(now_rfc3339())
    .bind(now_rfc3339())
    .execute(pool)
    .await
    .unwrap();

    crate::verification_methods::create_method(pool, backend, &seeded.fragment, &OLD)
        .await
        .unwrap();
    crate::db::query(&crate::db::adapt_sql(
        "INSERT INTO happyview_space_dids (id, did, signing_key_enc, rotation_key_enc, created_by, created_at) VALUES (?, ?, ?, ?, 'did:plc:owner', ?)",
        backend,
    ))
    .bind(&seeded.space_did_id)
    .bind(format!("did:plc:{}", seeded.space_did_id))
    .bind(encrypt(&OLD, b"space-signing").unwrap())
    .bind(encrypt(&OLD, b"space-rotation").unwrap())
    .bind(now_rfc3339())
    .execute(pool)
    .await
    .unwrap();
    crate::db::query(&crate::db::adapt_sql(
        "INSERT INTO happyview_space_authorities (did, signing_key_enc, kem_key_enc, rotation_key_enc, genesis_op, plc_state, created_by, created_at, updated_at) VALUES (?, ?, ?, ?, '{}', 'published', 'did:plc:owner', ?, ?)",
        backend,
    ))
    .bind(format!("did:plc:{}-authority", seeded.space_did_id))
    .bind(encrypt(&OLD, b"authority-signing").unwrap())
    .bind(encrypt(&OLD, b"authority-kem").unwrap())
    .bind(encrypt(&OLD, b"authority-rotation").unwrap())
    .bind(now_rfc3339())
    .bind(now_rfc3339())
    .execute(pool)
    .await
    .unwrap();

    for table in ["happyview_delegation_key", "happyview_attestation_key"] {
        crate::db::query(&format!("DELETE FROM {table}"))
            .execute(pool)
            .await
            .unwrap();
    }
    crate::spaces::delegation_key::load_or_create(pool, backend, &old())
        .await
        .unwrap();
    crate::plugin::attestation::load_or_generate(
        pool,
        backend,
        "https://rotation.example",
        Some(&old()),
    )
    .await
    .unwrap();

    crate::db::query("DELETE FROM happyview_script_variables")
        .execute(pool)
        .await
        .unwrap();
    let variable = crate::script::variables::seal(Some(&old()), "secret").unwrap();
    crate::db::query(&crate::db::adapt_sql(
        "INSERT INTO happyview_script_variables (key, value, encrypted, created_at) VALUES ('API_KEY', ?, ?, ?)",
        backend,
    ))
    .bind(&variable.value)
    .bind(variable.encrypted)
    .bind(now_rfc3339())
    .execute(pool)
    .await
    .unwrap();

    for table in [
        crate::auth::oauth_store::SESSIONS_TABLE,
        crate::linked_repos::client::LINKED_SESSIONS_TABLE,
    ] {
        DbSessionStore::new_with_table(pool.clone(), backend, table, Some(old()))
            .set(
                seeded.session_did.clone(),
                crate::auth::oauth_store::tests::sample_session("access"),
            )
            .await
            .unwrap();
    }
    DbStateStore::new(pool.clone(), backend, Some(old()))
        .set(
            seeded.state_key.clone(),
            crate::auth::oauth_store::tests::sample_state(),
        )
        .await
        .unwrap();
    seeded
}

/// The whole rotation: values sealed under the old key in every store, then
/// a re-seal with the new key current and the old one previous. Afterwards
/// nothing needs the previous key, the plaintext client key is sealed, and
/// every value opens under the new key alone.
#[tokio::test]
#[serial_test::serial(delegation_key, encryption_rotation)]
async fn a_reseal_leaves_nothing_needing_the_previous_key() {
    for (pool, backend) in test_pools().await {
        let seeded = seed_everything_under_the_old_key(&pool, backend).await;

        let before = crate::reseal::status(&pool, backend, &rotating())
            .await
            .unwrap();
        let count = |name: &str| before.iter().find(|s| s.store == name).unwrap().clone();
        for store in [
            "dpop_keys",
            "dpop_sessions",
            "oauth_client_keys",
            "external_account_tokens",
            "service_identity",
            "verification_methods",
            "space_dids",
            "space_authorities",
            "attestation_key",
            "delegation_key",
            "script_variables",
            "oauth_sessions",
            "linked_repo_sessions",
            "oauth_state",
            "plugin_secrets",
        ] {
            assert!(count(store).previous >= 1, "{store}: {:?}", count(store));
        }
        assert!(count("oauth_client_keys").unsealed >= 1);

        let (_, stopped) = crate::reseal::reseal(&pool, backend, &rotating(), || async { None })
            .await
            .unwrap();
        assert_eq!(stopped, None);

        let after = crate::reseal::status(&pool, backend, &rotating())
            .await
            .unwrap();
        for store in &after {
            assert_eq!((store.previous, store.unsealed), (0, 0), "{store:?}");
        }

        let new = new_only();
        crate::oauth::keys::get_dpop_key(&pool, backend, &new, &seeded.provision)
            .await
            .unwrap();
        let session = crate::oauth::sessions::get_dpop_session(
            &pool,
            backend,
            &new,
            &seeded.client,
            &seeded.did,
            &seeded.key_id,
        )
        .await
        .unwrap();
        assert_eq!(session.refresh_token.as_deref(), Some("refresh"));
        for owner in [&seeded.owner_sealed, &seeded.owner_plain] {
            assert_eq!(
                crate::oauth::client_keys::load_keys(&pool, backend, Some(&new), owner)
                    .await
                    .unwrap()
                    .len(),
                1
            );
        }
        let (plain_flag,): (i64,) = crate::db::query_as(&crate::db::adapt_sql(
            "SELECT CAST(encrypted AS BIGINT) FROM happyview_oauth_client_keys WHERE owner = ?",
            backend,
        ))
        .bind(&seeded.owner_plain)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(plain_flag, 1);
        assert_eq!(
            crate::external_auth::tokens::get_tokens(
                &pool,
                backend,
                Some(&new),
                &seeded.did,
                &seeded.plugin
            )
            .await
            .unwrap()
            .refresh_token
            .as_deref(),
            Some("refresh")
        );
        assert_eq!(
            crate::plugin::secrets::load_plugin_secrets(&pool, backend, Some(&new), &seeded.plugin)
                .await
                .get("API_KEY")
                .map(String::as_str),
            Some("shh")
        );
        let (signing, rotation): (String, String) = crate::db::query_as(
            "SELECT signing_key_enc, rotation_key_enc FROM happyview_service_identity",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(crate::plc::decrypt_key(&signing, &new).unwrap(), b"signing");
        assert_eq!(
            crate::plc::decrypt_key(&rotation, &new).unwrap(),
            b"rotation"
        );
        assert!(
            crate::verification_methods::get_private_key_bytes(
                &pool,
                backend,
                &seeded.fragment,
                &new
            )
            .await
            .unwrap()
            .is_some()
        );
        let (space_signing,): (Vec<u8>,) = crate::db::query_as(&crate::db::adapt_sql(
            "SELECT signing_key_enc FROM happyview_space_dids WHERE id = ?",
            backend,
        ))
        .bind(&seeded.space_did_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(new.decrypt(&space_signing).unwrap(), b"space-signing");
        let (signing, kem, rotation): (Vec<u8>, Vec<u8>, Vec<u8>) =
            crate::db::query_as(&crate::db::adapt_sql(
                "SELECT signing_key_enc, kem_key_enc, rotation_key_enc FROM happyview_space_authorities WHERE did = ?",
                backend,
            ))
            .bind(format!("did:plc:{}-authority", seeded.space_did_id))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(new.decrypt(&signing).unwrap(), b"authority-signing");
        assert_eq!(new.decrypt(&kem).unwrap(), b"authority-kem");
        assert_eq!(new.decrypt(&rotation).unwrap(), b"authority-rotation");
        assert!(
            crate::spaces::delegation_key::load(&pool, backend, &new)
                .await
                .unwrap()
                .is_some()
        );
        crate::plugin::attestation::load_or_generate(
            &pool,
            backend,
            "https://rotation.example",
            Some(&new),
        )
        .await
        .unwrap();
        assert_eq!(
            crate::script::variables::load(&pool, backend, Some(&new)).await["API_KEY"],
            "secret"
        );
        for table in [
            crate::auth::oauth_store::SESSIONS_TABLE,
            crate::linked_repos::client::LINKED_SESSIONS_TABLE,
        ] {
            assert!(
                DbSessionStore::new_with_table(pool.clone(), backend, table, Some(new))
                    .get(&seeded.session_did)
                    .await
                    .unwrap()
                    .is_some(),
                "{table}"
            );
        }
        assert!(
            DbStateStore::new(pool.clone(), backend, Some(new))
                .get(&seeded.state_key)
                .await
                .unwrap()
                .is_some()
        );
    }
}

/// A value rewritten between the re-seal's read and its write wins: the
/// writer sealed it under the current key already, and re-sealing what was
/// read would put the old value back.
#[tokio::test]
#[serial_test::serial(encryption_rotation)]
async fn a_reseal_leaves_a_value_that_changed_since_it_was_read() {
    for (pool, backend) in test_pools().await {
        crate::db::query("DELETE FROM happyview_script_variables")
            .execute(&pool)
            .await
            .unwrap();
        let stale = crate::script::variables::seal(Some(&old()), "stale").unwrap();
        crate::db::query(&crate::db::adapt_sql(
            "INSERT INTO happyview_script_variables (key, value, encrypted, created_at) VALUES ('API_KEY', ?, 1, ?)",
            backend,
        ))
        .bind(&stale.value)
        .bind(now_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        // The writer's save lands after the pass read the row.
        let fresh = crate::script::variables::seal(Some(&rotating()), "fresh").unwrap();
        let wrote = crate::reseal::tests::reseal_with_a_write_between(
            &pool,
            backend,
            &rotating(),
            "script_variables",
            || async {
                crate::db::query(&crate::db::adapt_sql(
                    "UPDATE happyview_script_variables SET value = ? WHERE key = 'API_KEY'",
                    backend,
                ))
                .bind(&fresh.value)
                .execute(&pool)
                .await
                .unwrap();
            },
        )
        .await;
        assert!(!wrote);
        assert_eq!(
            crate::script::variables::load(&pool, backend, Some(&new_only())).await["API_KEY"],
            "fresh"
        );
    }
}
