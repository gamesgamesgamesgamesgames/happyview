//! Core record-write service functions for spaces, shared by the HTTP handlers
//! (`src/spaces/routes.rs`) and (in a later task) a Lua binding, so both call
//! the exact same membership/validation/write logic.

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::lua::tid::generate_tid;
use crate::spaces::types::*;
use crate::spaces::{SpaceUri, db, members};
use sha2::{Digest, Sha256};

pub(crate) async fn resolve_space(state: &AppState, space_ref: &str) -> Result<Space, AppError> {
    let uri = SpaceUri::parse(space_ref)?;
    db::get_space_by_address(
        &state.db,
        state.db_backend,
        &uri.did,
        &uri.type_nsid,
        &uri.skey,
    )
    .await?
    .ok_or_else(|| AppError::NotFound("Space not found".into()))
}

pub(crate) fn content_cid(record: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(record).unwrap_or_default();
    let hash = Sha256::digest(&bytes);
    format!("bafyrei{}", hex::encode(&hash[..20]))
}

pub(crate) async fn require_space_admin(
    state: &AppState,
    space: &Space,
    did: &str,
) -> Result<(), AppError> {
    if space.authority_did == did {
        return Ok(());
    }
    let sql = adapt_sql(
        "SELECT is_super FROM happyview_users WHERE did = ?",
        state.db_backend,
    );
    let row: Option<(i32,)> = crate::db::query_as(&sql)
        .bind(did)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to check admin status: {e}")))?;
    if row.is_some_and(|(is_super,)| is_super != 0) {
        return Ok(());
    }
    Err(AppError::Forbidden(
        "Only the space authority can perform this action".into(),
    ))
}

pub(crate) async fn require_membership(
    state: &AppState,
    space: &Space,
    did: &str,
    require_write: bool,
    space_credential: Option<&str>,
) -> Result<SpaceAccess, AppError> {
    if let Some(token) = space_credential {
        let space_uri = format!(
            "at://{}/space/{}/{}",
            space.did, space.type_nsid, space.skey
        );
        match crate::spaces::credential::verify_external_credential(
            token,
            &state.http,
            &state.config.plc_url,
        )
        .await
        {
            Ok(claims) if claims.sub == space_uri => {
                if crate::spaces::routes::space_credential_revoked(state, token).await? {
                    // fall through
                } else if require_write {
                    return Err(AppError::Forbidden(
                        "Write access is required for this action".into(),
                    ));
                } else {
                    return Ok(SpaceAccess::Read);
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    let access = members::is_member(&state.db, state.db_backend, &space.id, did)
        .await?
        .ok_or_else(|| AppError::Forbidden("You are not a member of this space".into()))?;
    if require_write && !access.can_write() {
        return Err(AppError::Forbidden(
            "Write access is required for this action".into(),
        ));
    }
    Ok(access)
}

pub(crate) async fn create_record(
    state: &AppState,
    did: &str,
    space_credential: Option<&str>,
    space_ref: &str,
    collection: &str,
    record: serde_json::Value,
) -> Result<(String, String), AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_membership(state, &space, did, true, space_credential).await?;

    let rkey = generate_tid();
    let cid = content_cid(&record);
    let record_uri = format!(
        "at://{}/space/{}/{}/{}/{}/{}",
        space.did, space.type_nsid, space.skey, did, collection, rkey
    );
    let rec = SpaceRecord {
        uri: record_uri.clone(),
        space_id: space.id.clone(),
        author_did: did.to_string(),
        collection: collection.to_string(),
        rkey,
        record,
        cid: cid.clone(),
        indexed_at: now_rfc3339(),
    };
    db::insert_space_record(&state.db, state.db_backend, &rec).await?;
    let rev = generate_tid();
    db::update_space_revision(&state.db, state.db_backend, &space.id, &rev).await?;
    Ok((record_uri, cid))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn put_record(
    state: &AppState,
    did: &str,
    space_credential: Option<&str>,
    space_ref: &str,
    collection: &str,
    rkey: &str,
    record: serde_json::Value,
    swap_cid: Option<String>,
) -> Result<(String, String), AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_membership(state, &space, did, true, space_credential).await?;

    let cid = content_cid(&record);
    let record_uri = format!(
        "at://{}/space/{}/{}/{}/{}/{}",
        space.did, space.type_nsid, space.skey, did, collection, rkey
    );
    let rec = SpaceRecord {
        uri: record_uri.clone(),
        space_id: space.id.clone(),
        author_did: did.to_string(),
        collection: collection.to_string(),
        rkey: rkey.to_string(),
        record,
        cid: cid.clone(),
        indexed_at: now_rfc3339(),
    };
    if let Some(swap) = swap_cid {
        db::upsert_space_record_with_swap(&state.db, state.db_backend, &rec, &swap).await?;
    } else {
        db::upsert_space_record(&state.db, state.db_backend, &rec).await?;
    }
    let rev = generate_tid();
    db::update_space_revision(&state.db, state.db_backend, &space.id, &rev).await?;
    Ok((record_uri, cid))
}

pub(crate) async fn delete_record(
    state: &AppState,
    did: &str,
    space_ref: &str,
    collection: &str,
    rkey: &str,
    swap_cid: Option<String>,
) -> Result<(), AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_membership(state, &space, did, true, None).await?;

    let record_uri = format!(
        "at://{}/space/{}/{}/{}/{}/{}",
        space.did, space.type_nsid, space.skey, did, collection, rkey
    );
    if let Some(swap) = swap_cid {
        db::delete_space_record_with_swap(&state.db, state.db_backend, &record_uri, &swap).await?;
    } else {
        let record = db::get_space_record(&state.db, state.db_backend, &record_uri).await?;
        match record {
            Some(r) if r.author_did != did => {
                return Err(AppError::Forbidden(
                    "You can only delete your own records".into(),
                ));
            }
            None => return Err(AppError::NotFound("Record not found".into())),
            _ => {}
        }
        db::delete_space_record(&state.db, state.db_backend, &record_uri).await?;
    }
    let rev = generate_tid();
    db::update_space_revision(&state.db, state.db_backend, &space.id, &rev).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_space(
    state: &AppState,
    did: &str,
    type_nsid: &str,
    skey: &str,
    display_name: Option<String>,
    description: Option<String>,
    mint_policy: Option<MintPolicy>,
    app_access: Option<AppAccess>,
    managing_app_did: Option<String>,
    config: Option<SpaceConfig>,
) -> Result<Space, AppError> {
    if type_nsid.is_empty() || skey.is_empty() {
        return Err(AppError::BadRequest("type and skey are required".into()));
    }
    let existing =
        db::get_space_by_address(&state.db, state.db_backend, did, type_nsid, skey).await?;
    if existing.is_some() {
        return Err(AppError::Conflict(
            "A space with this address already exists".into(),
        ));
    }
    let mut config = config.unwrap_or_default();
    if let Some(decl) = state.lexicons.get_space_declaration(type_nsid).await
        && let Some(collections) = decl.space_collections
        && !collections.is_empty()
        && !config.extra.contains_key("allowedCollections")
    {
        config.extra.insert(
            "allowedCollections".to_string(),
            serde_json::Value::Array(
                collections
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    let space = Space {
        id: uuid::Uuid::new_v4().to_string(),
        did: did.to_string(),
        authority_did: did.to_string(),
        creator_did: did.to_string(),
        type_nsid: type_nsid.to_string(),
        skey: skey.to_string(),
        display_name,
        description,
        mint_policy: mint_policy.unwrap_or(MintPolicy::MemberList),
        app_access: app_access.unwrap_or_default(),
        managing_app_did,
        config,
        revision: None,
        created_at: now_rfc3339(),
        updated_at: now_rfc3339(),
    };
    db::create_space(&state.db, state.db_backend, &space).await?;
    if let Some(encryption_key) = &state.config.token_encryption_key
        && let Err(e) = crate::verification_methods::ensure_atproto_space_method(
            &state.db,
            state.db_backend,
            encryption_key,
        )
        .await
    {
        tracing::warn!("failed to auto-provision #atproto_space verification method: {e}");
    }
    let member = SpaceMember {
        id: uuid::Uuid::new_v4().to_string(),
        space_id: space.id.clone(),
        did: did.to_string(),
        access: SpaceAccess::Write,
        is_delegation: false,
        granted_by: Some(did.to_string()),
        created_at: now_rfc3339(),
    };
    db::add_member(&state.db, state.db_backend, &member).await?;
    Ok(space)
}

pub(crate) async fn add_member(
    state: &AppState,
    actor_did: &str,
    space_ref: &str,
    member_did: &str,
    access: Option<SpaceAccess>,
    is_delegation: Option<bool>,
) -> Result<SpaceMember, AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_space_admin(state, &space, actor_did).await?;
    if db::get_member(&state.db, state.db_backend, &space.id, member_did)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict(
            "Member already exists in this space".into(),
        ));
    }
    let member = SpaceMember {
        id: uuid::Uuid::new_v4().to_string(),
        space_id: space.id,
        did: member_did.to_string(),
        access: access.unwrap_or(SpaceAccess::Read),
        is_delegation: is_delegation.unwrap_or(false),
        granted_by: Some(actor_did.to_string()),
        created_at: now_rfc3339(),
    };
    db::add_member(&state.db, state.db_backend, &member).await?;
    Ok(member)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn update_space(
    state: &AppState,
    actor_did: &str,
    space_ref: &str,
    display_name: Option<Option<String>>,
    description: Option<Option<String>>,
    mint_policy: Option<MintPolicy>,
    app_access: Option<AppAccess>,
    managing_app_did: Option<Option<String>>,
    config: Option<SpaceConfig>,
) -> Result<Space, AppError> {
    let mut space = resolve_space(state, space_ref).await?;
    require_space_admin(state, &space, actor_did).await?;
    if let Some(name) = display_name {
        space.display_name = name;
    }
    if let Some(desc) = description {
        space.description = desc;
    }
    if let Some(policy) = mint_policy {
        space.mint_policy = policy;
    }
    if let Some(access) = app_access {
        space.app_access = access;
    }
    if let Some(did) = managing_app_did {
        space.managing_app_did = did;
    }
    if let Some(cfg) = config {
        space.config = cfg;
    }
    db::update_space(&state.db, state.db_backend, &space).await?;
    Ok(space)
}

pub(crate) async fn delete_space(
    state: &AppState,
    actor_did: &str,
    space_ref: &str,
) -> Result<(), AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_space_admin(state, &space, actor_did).await?;
    db::delete_space(&state.db, state.db_backend, &space.id).await?;
    Ok(())
}

pub(crate) async fn remove_member(
    state: &AppState,
    actor_did: &str,
    space_ref: &str,
    member_did: &str,
) -> Result<(), AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_space_admin(state, &space, actor_did).await?;
    db::revoke_space_credentials_for_member(&state.db, state.db_backend, &space.id, member_did)
        .await?;
    let removed = db::remove_member(&state.db, state.db_backend, &space.id, member_did).await?;
    if !removed {
        return Err(AppError::NotFound("Member not found in this space".into()));
    }
    Ok(())
}

pub(crate) async fn create_invite(
    state: &AppState,
    actor_did: &str,
    space_ref: &str,
    access: Option<SpaceAccess>,
    max_uses: Option<i64>,
    expires_at: Option<String>,
) -> Result<(SpaceInvite, String), AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_space_admin(state, &space, actor_did).await?;
    let mut token_bytes = [0u8; 24];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut token_bytes);
    let token = hex::encode(token_bytes);
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let invite = SpaceInvite {
        id: uuid::Uuid::new_v4().to_string(),
        space_id: space.id,
        token_hash,
        created_by: actor_did.to_string(),
        access: access.unwrap_or(SpaceAccess::Read),
        max_uses,
        uses: 0,
        expires_at,
        revoked: false,
        created_at: now_rfc3339(),
    };
    db::create_invite(&state.db, state.db_backend, &invite).await?;
    Ok((invite, token))
}

pub(crate) async fn accept_invite(
    state: &AppState,
    did: &str,
    token: &str,
) -> Result<(String, SpaceAccess), AppError> {
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let invite = db::get_invite_by_token_hash(&state.db, state.db_backend, &token_hash)
        .await?
        .ok_or_else(|| AppError::NotFound("Invalid invite token".into()))?;
    if invite.revoked {
        return Err(AppError::BadRequest("This invite has been revoked".into()));
    }
    if let Some(max) = invite.max_uses
        && invite.uses >= max
    {
        return Err(AppError::BadRequest(
            "This invite has reached its maximum uses".into(),
        ));
    }
    if let Some(ref expires) = invite.expires_at
        && now_rfc3339() > *expires
    {
        return Err(AppError::BadRequest("This invite has expired".into()));
    }
    if db::get_member(&state.db, state.db_backend, &invite.space_id, did)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict(
            "You are already a member of this space".into(),
        ));
    }
    let member = SpaceMember {
        id: uuid::Uuid::new_v4().to_string(),
        space_id: invite.space_id.clone(),
        did: did.to_string(),
        access: invite.access,
        is_delegation: false,
        granted_by: Some(invite.created_by.clone()),
        created_at: now_rfc3339(),
    };
    db::add_member(&state.db, state.db_backend, &member).await?;
    db::increment_invite_uses(&state.db, state.db_backend, &invite.id).await?;
    let space = db::get_space(&state.db, state.db_backend, &invite.space_id).await?;
    let space_uri = space
        .map(|s| format!("at://{}/space/{}/{}", s.did, s.type_nsid, s.skey))
        .ok_or_else(|| AppError::Internal("space vanished after member insert".into()))?;
    Ok((space_uri, member.access))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::DatabaseBackend;
    use crate::lexicon::LexiconRegistry;
    use tokio::sync::watch;

    /// These are integration tests requiring Postgres, mirroring the skip
    /// idiom used by `tests/common`'s `require_db!` macro: when
    /// `TEST_DATABASE_URL` isn't set, the test returns early instead of
    /// failing, so `cargo test --lib` stays DB-free. Run for real via:
    /// `TEST_DATABASE_URL=postgres://... cargo test -p happyview spaces::service`
    macro_rules! require_test_db {
        () => {
            if std::env::var("TEST_DATABASE_URL").is_err() {
                eprintln!("skipped (TEST_DATABASE_URL not set)");
                return;
            }
        };
    }

    /// Build an `AppState` backed by a migrated, empty Postgres database
    /// (`TEST_DATABASE_URL`). Callers must have already checked
    /// `require_test_db!()`.
    async fn service_empty_db() -> AppState {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must be set for spaces::service integration tests");
        let backend = DatabaseBackend::from_url(&url);
        let pool = crate::db::connect(&url, backend).await;

        let config = Config {
            host: "127.0.0.1".into(),
            port: 0,
            database_url: String::new(),
            database_backend: backend,
            public_url: String::new(),
            session_secret: "test-secret".into(),
            jetstream_url: String::new(),
            relay_url: String::new(),
            plc_url: String::new(),
            static_dir: String::new(),
            base_path: None,
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
        };
        let (collections_tx, _) = watch::channel(vec![]);
        let (labeler_subscriptions_tx, _) = watch::channel(());

        let atrium_http = std::sync::Arc::new(atrium_oauth::DefaultHttpClient::default());
        let did_resolver = atrium_identity::did::CommonDidResolver::new(
            atrium_identity::did::CommonDidResolverConfig {
                plc_directory_url: "https://plc.directory".into(),
                http_client: std::sync::Arc::clone(&atrium_http),
            },
        );
        let handle_resolver = atrium_identity::handle::AtprotoHandleResolver::new(
            atrium_identity::handle::AtprotoHandleResolverConfig {
                dns_txt_resolver: crate::dns::NativeDnsResolver::new(),
                http_client: atrium_http,
            },
        );
        let oauth = atrium_oauth::OAuthClient::new(atrium_oauth::OAuthClientConfig {
            client_metadata: atrium_oauth::AtprotoLocalhostClientMetadata {
                redirect_uris: Some(vec!["http://127.0.0.1:0/auth/callback".into()]),
                scopes: Some(vec![atrium_oauth::Scope::Known(
                    atrium_oauth::KnownScope::Atproto,
                )]),
            },
            keys: None,
            state_store: crate::auth::oauth_store::DbStateStore::new(pool.clone(), backend),
            session_store: crate::auth::oauth_store::DbSessionStore::new(pool.clone(), backend),
            resolver: atrium_oauth::OAuthResolverConfig {
                did_resolver,
                handle_resolver,
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
        })
        .expect("Failed to create test OAuth client");

        AppState {
            config,
            http: reqwest::Client::new(),
            db: pool.clone(),
            backfill_db: pool.clone(),
            db_backend: backend,
            domain_cache: crate::domain::DomainCache::new(),
            lexicons: LexiconRegistry::new(),
            collections_tx,
            labeler_subscriptions_tx,
            rate_limiter: crate::rate_limit::RateLimiter::new(
                crate::rate_limit::RateLimitDefaults {
                    query_cost: 1,
                    procedure_cost: 1,
                    proxy_cost: 1,
                },
            ),
            oauth: std::sync::Arc::new(crate::auth::OAuthClientRegistry::new(std::sync::Arc::new(
                oauth,
            ))),
            oauth_state_store: crate::auth::oauth_store::DbStateStore::new(pool.clone(), backend),
            cookie_key: axum_extra::extract::cookie::Key::derive_from(
                b"test-secret-for-tests-only-not-production",
            ),
            plugin_registry: std::sync::Arc::new(crate::plugin::PluginRegistry::new()),
            wasm_runtime: std::sync::Arc::new(
                crate::plugin::WasmRuntime::new().expect("wasm runtime"),
            ),
            attestation_signer: None,
            official_registry: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::plugin::official_registry::OfficialRegistryState::default(),
            )),
            official_registry_config: crate::plugin::official_registry::RegistryConfig::production(
            ),
            proxy_config: std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(
                crate::proxy_config::ProxyConfig::default(),
            ))),
            backfill_events_tx: tokio::sync::broadcast::channel(16).0,
            verbose_event_logging: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Build a migrated Postgres-backed `AppState` seeded with one space and
    /// one write-member (also the space's `authority_did`, for later tasks).
    /// Returns `(state, space_uri, member_did)`.
    ///
    /// Uses randomised DIDs/skeys so parallel tests sharing the same
    /// `TEST_DATABASE_URL` database don't collide (no truncation is needed).
    async fn service_test_db() -> (AppState, String, String) {
        let state = service_empty_db().await;

        let unique = uuid::Uuid::new_v4().simple().to_string();
        let space_id = uuid::Uuid::new_v4().to_string();
        let member_did = format!("did:plc:writer{unique}");
        let space_did = format!("did:plc:owner{unique}");
        let type_nsid = "com.example.forum";
        let skey = format!("main{unique}");

        let space = Space {
            id: space_id.clone(),
            did: space_did.clone(),
            authority_did: member_did.clone(),
            creator_did: member_did.clone(),
            type_nsid: type_nsid.to_string(),
            skey: skey.clone(),
            display_name: None,
            description: None,
            mint_policy: MintPolicy::MemberList,
            app_access: AppAccess::default(),
            managing_app_did: None,
            config: SpaceConfig::default(),
            revision: None,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
        };
        db::create_space(&state.db, state.db_backend, &space)
            .await
            .expect("failed to seed test space");

        let member = SpaceMember {
            id: uuid::Uuid::new_v4().to_string(),
            space_id: space_id.clone(),
            did: member_did.clone(),
            access: SpaceAccess::Write,
            is_delegation: false,
            granted_by: None,
            created_at: now_rfc3339(),
        };
        db::add_member(&state.db, state.db_backend, &member)
            .await
            .expect("failed to seed test member");

        let space_uri = format!("at://{space_did}/space/{type_nsid}/{skey}");
        (state, space_uri, member_did)
    }

    /// `delete_record` builds the record URI from the *caller's own* DID
    /// (see the `record_uri` construction in `service::delete_record`), so a
    /// second write-member can never naturally collide with another
    /// author's URI through the normal create/put/delete API surface — both
    /// segments are always the same `did`. To exercise the ownership check
    /// (`r.author_did != did` -> Forbidden) at all we have to manufacture a
    /// data state where the URI's embedded DID segment (member B) diverges
    /// from the row's stored `author_did` (member A), by seeding the record
    /// directly via `db::insert_space_record` rather than going through
    /// `create_record`/`put_record`. This still exercises the real
    /// `delete_record` ownership-check code path.
    #[tokio::test]
    async fn delete_record_forbidden_for_non_author() {
        require_test_db!();
        let (state, space_uri, member_a) = service_test_db().await; // member_a is a write-member (and authority)
        let member_b = format!("did:plc:writerB{}", uuid::Uuid::new_v4().simple());
        super::add_member(
            &state,
            &member_a,
            &space_uri,
            &member_b,
            Some(SpaceAccess::Write),
            None,
        )
        .await
        .expect("authority may add member B as a write-member");

        let space = super::resolve_space(&state, &space_uri).await.unwrap();
        let collection = "com.example.item";
        let rkey = "fixedrkey-ownership";
        // This is the exact URI `delete_record` will construct when member_b
        // calls it with this collection/rkey.
        let record_uri = format!(
            "at://{}/space/{}/{}/{}/{}/{}",
            space.did, space.type_nsid, space.skey, member_b, collection, rkey
        );
        let content = serde_json::json!({ "text": "authored by A" });
        let rec = SpaceRecord {
            uri: record_uri.clone(),
            space_id: space.id.clone(),
            author_did: member_a.clone(), // stored author is A, not B
            collection: collection.to_string(),
            rkey: rkey.to_string(),
            record: content.clone(),
            cid: super::content_cid(&content),
            indexed_at: now_rfc3339(),
        };
        db::insert_space_record(&state.db, state.db_backend, &rec)
            .await
            .expect("failed to seed mismatched-author record");

        let err = super::delete_record(&state, &member_b, &space_uri, collection, rkey, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, crate::error::AppError::Forbidden(_)),
            "expected Forbidden, got: {err:?}"
        );
        // record must still be present since the delete was rejected
        assert!(
            db::get_space_record(&state.db, state.db_backend, &record_uri)
                .await
                .unwrap()
                .is_some()
        );
    }

    /// A caller who is not a member of the space at all must be rejected
    /// with `Forbidden` (write-membership check), not `NotFound`. Before the
    /// membership check was added, `delete_record` would build the record
    /// URI from the *caller's own* DID, fail to find a row at that URI (since
    /// the non-member never authored anything), and return `NotFound`
    /// instead — masking the fact that non-members should never be allowed
    /// to reach the ownership check at all.
    #[tokio::test]
    async fn delete_record_rejects_non_member() {
        require_test_db!();
        let (state, space_uri, _member_did) = service_test_db().await;
        let stranger = format!("did:plc:stranger{}", uuid::Uuid::new_v4().simple());

        let err = super::delete_record(
            &state,
            &stranger,
            &space_uri,
            "com.example.item",
            "rk1",
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, crate::error::AppError::Forbidden(_)),
            "expected Forbidden, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn accept_invite_rejects_revoked() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let (invite, token) =
            super::create_invite(&state, &authority, &space_uri, None, None, None)
                .await
                .expect("authority creates invite");
        let revoked = db::revoke_invite(&state.db, state.db_backend, &invite.id)
            .await
            .expect("revoke should succeed");
        assert!(revoked);

        let err = super::accept_invite(&state, "did:plc:joiner-revoked", &token)
            .await
            .unwrap_err();
        match err {
            crate::error::AppError::BadRequest(msg) => {
                assert!(
                    msg.to_lowercase().contains("revoked"),
                    "expected 'revoked' in message, got: {msg}"
                );
            }
            other => panic!("expected BadRequest, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn accept_invite_rejects_expired() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let (_invite, token) = super::create_invite(
            &state,
            &authority,
            &space_uri,
            None,
            None,
            Some("2000-01-01T00:00:00+00:00".to_string()),
        )
        .await
        .expect("authority creates invite");

        let err = super::accept_invite(&state, "did:plc:joiner-expired", &token)
            .await
            .unwrap_err();
        match err {
            crate::error::AppError::BadRequest(msg) => {
                assert!(
                    msg.to_lowercase().contains("expired"),
                    "expected 'expired' in message, got: {msg}"
                );
            }
            other => panic!("expected BadRequest, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn accept_invite_rejects_after_max_uses_exhausted() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let (_invite, token) =
            super::create_invite(&state, &authority, &space_uri, None, Some(1), None)
                .await
                .expect("authority creates invite");

        super::accept_invite(&state, "did:plc:joiner-x", &token)
            .await
            .expect("first accept (X) should succeed");

        let err = super::accept_invite(&state, "did:plc:joiner-y", &token)
            .await
            .unwrap_err();
        match err {
            crate::error::AppError::BadRequest(msg) => {
                assert!(
                    msg.to_lowercase().contains("maximum uses"),
                    "expected 'maximum uses' in message, got: {msg}"
                );
            }
            other => panic!("expected BadRequest, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn accept_invite_rejects_already_member() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let (_invite, token) =
            super::create_invite(&state, &authority, &space_uri, None, None, None)
                .await
                .expect("authority creates invite");

        super::accept_invite(&state, "did:plc:joiner-twice", &token)
            .await
            .expect("first accept should succeed");

        let err = super::accept_invite(&state, "did:plc:joiner-twice", &token)
            .await
            .unwrap_err();
        assert!(
            matches!(err, crate::error::AppError::Conflict(_)),
            "expected Conflict, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn put_record_upserts_then_rejects_swap_mismatch() {
        require_test_db!();
        let (state, space_uri, member_did) = service_test_db().await;
        let collection = "com.example.item";
        let rkey = "fixedrkey-put";

        let (uri1, cid1) = super::put_record(
            &state,
            &member_did,
            None,
            &space_uri,
            collection,
            rkey,
            serde_json::json!({ "text": "v1" }),
            None,
        )
        .await
        .expect("first put_record should create the record");

        let (uri2, cid2) = super::put_record(
            &state,
            &member_did,
            None,
            &space_uri,
            collection,
            rkey,
            serde_json::json!({ "text": "v2" }),
            None,
        )
        .await
        .expect("second put_record should update the existing record");

        assert_eq!(
            uri1, uri2,
            "put_record on the same rkey should be idempotent on URI"
        );
        assert_ne!(cid1, cid2, "content changed, so the CID should change");

        let stored = db::get_space_record(&state.db, state.db_backend, &uri1)
            .await
            .unwrap()
            .expect("record should exist");
        assert_eq!(stored.record, serde_json::json!({ "text": "v2" }));
        assert_eq!(stored.cid, cid2);

        // confirm only one row exists for this collection (no duplicate insert)
        let space = super::resolve_space(&state, &space_uri).await.unwrap();
        let (records, _cursor) = db::list_space_records(
            &state.db,
            state.db_backend,
            &space.id,
            None,
            Some(collection),
            100,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(records.len(), 1, "expected exactly one row after upsert");

        // wrong swap_cid should fail with a CID-mismatch Conflict
        let err = super::put_record(
            &state,
            &member_did,
            None,
            &space_uri,
            collection,
            rkey,
            serde_json::json!({ "text": "v3" }),
            Some("bafyreiwrongwrongwrongwrongwrong".to_string()),
        )
        .await
        .unwrap_err();
        match err {
            crate::error::AppError::Conflict(msg) => {
                assert!(
                    msg.contains("CID mismatch"),
                    "expected 'CID mismatch' in message, got: {msg}"
                );
            }
            other => panic!("expected Conflict, got: {other:?}"),
        }
        // content must be unchanged after the rejected swap
        let stored = db::get_space_record(&state.db, state.db_backend, &uri1)
            .await
            .unwrap()
            .expect("record should still exist");
        assert_eq!(stored.record, serde_json::json!({ "text": "v2" }));
    }

    #[tokio::test]
    async fn delete_record_with_swap_cid() {
        require_test_db!();
        let (state, space_uri, member_did) = service_test_db().await;
        let collection = "com.example.item";
        let rkey = "fixedrkey-del";

        let (uri, cid) = super::put_record(
            &state,
            &member_did,
            None,
            &space_uri,
            collection,
            rkey,
            serde_json::json!({ "text": "to-delete" }),
            None,
        )
        .await
        .expect("put_record should create the record");

        // wrong swap_cid -> Conflict (record exists, per db::delete_space_record_with_swap)
        let err = super::delete_record(
            &state,
            &member_did,
            &space_uri,
            collection,
            rkey,
            Some("bafyreiwrongwrongwrongwrongwrong".to_string()),
        )
        .await
        .unwrap_err();
        match err {
            crate::error::AppError::Conflict(msg) => {
                assert!(
                    msg.contains("CID mismatch"),
                    "expected 'CID mismatch' in message, got: {msg}"
                );
            }
            other => panic!("expected Conflict, got: {other:?}"),
        }
        // record must still be present
        assert!(
            db::get_space_record(&state.db, state.db_backend, &uri)
                .await
                .unwrap()
                .is_some()
        );

        // correct swap_cid -> deletes
        super::delete_record(&state, &member_did, &space_uri, collection, rkey, Some(cid))
            .await
            .expect("delete with correct swap_cid should succeed");
        assert!(
            db::get_space_record(&state.db, state.db_backend, &uri)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn create_record_inserts_and_bumps_revision() {
        require_test_db!();
        let (state, space_uri, member_did) = service_test_db().await; // space with member_did as write-member
        let (uri, cid) = super::create_record(
            &state,
            &member_did,
            None,
            &space_uri,
            "com.example.item",
            serde_json::json!({ "text": "hello" }),
        )
        .await
        .expect("write should succeed for a write-member");
        assert!(uri.starts_with("at://"));
        assert!(cid.starts_with("bafyrei"));
        // revision bumped
        let space = super::resolve_space(&state, &space_uri).await.unwrap();
        assert!(space.revision.is_some());
    }

    #[tokio::test]
    async fn create_record_rejects_non_member() {
        require_test_db!();
        let (state, space_uri, _member) = service_test_db().await;
        let err = super::create_record(
            &state,
            "did:plc:stranger",
            None,
            &space_uri,
            "com.example.item",
            serde_json::json!({ "text": "no" }),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, crate::error::AppError::Forbidden(_)));
    }

    #[tokio::test]
    async fn create_space_inserts_and_adds_creator_as_writer() {
        require_test_db!();
        let state = service_empty_db().await; // migrated DB, no spaces
        let space = super::create_space(
            &state,
            "did:plc:creator",
            "com.example.chat",
            "general",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create should succeed");
        assert_eq!(space.authority_did, "did:plc:creator");
        let access = crate::spaces::members::is_member(
            &state.db,
            state.db_backend,
            &space.id,
            "did:plc:creator",
        )
        .await
        .unwrap();
        assert_eq!(access, Some(crate::spaces::types::SpaceAccess::Write));
    }

    #[tokio::test]
    async fn delete_space_requires_admin() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let err = super::delete_space(&state, "did:plc:notadmin", &space_uri)
            .await
            .unwrap_err();
        assert!(matches!(err, crate::error::AppError::Forbidden(_)));
        super::delete_space(&state, &authority, &space_uri)
            .await
            .expect("authority may delete");
        assert!(super::resolve_space(&state, &space_uri).await.is_err()); // gone
    }

    #[tokio::test]
    async fn add_member_requires_admin() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await; // authority is the space authority_did
        // authority can add
        super::add_member(
            &state,
            &authority,
            &space_uri,
            "did:plc:newbie",
            Some(crate::spaces::types::SpaceAccess::Write),
            None,
        )
        .await
        .expect("authority may add members");
        // a non-admin cannot
        let err = super::add_member(
            &state,
            "did:plc:randomer",
            &space_uri,
            "did:plc:x",
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, crate::error::AppError::Forbidden(_)));
    }

    #[tokio::test]
    async fn invite_roundtrip() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let (_invite, token) = super::create_invite(
            &state,
            &authority,
            &space_uri,
            Some(crate::spaces::types::SpaceAccess::Write),
            None,
            None,
        )
        .await
        .expect("authority creates invite");
        let (joined_uri, access) = super::accept_invite(&state, "did:plc:joiner", &token)
            .await
            .expect("joiner redeems invite");
        assert_eq!(joined_uri, space_uri);
        assert_eq!(access, crate::spaces::types::SpaceAccess::Write);
        // now a member with write access
        let space = super::resolve_space(&state, &space_uri).await.unwrap();
        let acc = crate::spaces::members::is_member(
            &state.db,
            state.db_backend,
            &space.id,
            "did:plc:joiner",
        )
        .await
        .unwrap();
        assert_eq!(acc, Some(crate::spaces::types::SpaceAccess::Write));
    }
}
