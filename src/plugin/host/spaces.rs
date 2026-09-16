//! Permissioned spaces, from a library plugin. Every function here is also
//! what the Lua `atproto.spaces` global calls for the pieces specific to its
//! Lua bindings — the feature-flag gate, the `access` shorthand parser,
//! `update`'s three-way patch, and query shaping — so a library and a script
//! can never see the flag, a parsed access level, or a page of records
//! differently. Every other write goes straight to `spaces::service`, the
//! single implementation both a script and a library call into.

use crate::AppState;
use crate::error::AppError;
use crate::spaces::service;
use crate::spaces::types::{AppAccess, MemberAccess, Policy, Space, SpaceConfig, SpaceRecord};

use happyview_plugin_sdk::wire::{
    Patch, RecordRef, SpaceInfo, SpaceInviteInfo, SpaceMemberAdd, SpaceMemberInfo,
    SpaceMemberRemove, SpaceRecordDelete, SpaceRecordInfo, SpaceRecordPut, SpaceRecordWrite,
    SpaceRecordsPage, SpaceUpdate, SpacesAcceptInvite, SpacesAccess, SpacesCreate, SpacesInfo,
    SpacesMembers, SpacesQuery,
};

#[derive(Debug, thiserror::Error)]
pub enum SpacesError {
    #[error("spaces feature is not enabled")]
    Disabled,
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    NotAuthorized(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Pds(String),
    #[error("{0}")]
    BadInput(String),
    #[error("{0}")]
    Other(String),
}

impl SpacesError {
    /// The envelope code a guest sees. One code per `AppError` variant, so a
    /// script can branch on `code` without caring which host operation
    /// produced it.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "SPACES_DISABLED",
            Self::NotFound(_) => "NOT_FOUND",
            Self::NotAuthorized(_) => "NOT_AUTHORIZED",
            Self::Conflict(_) => "CONFLICT",
            Self::Pds(_) => "PDS_ERROR",
            Self::BadInput(_) => "BAD_INPUT",
            Self::Other(_) => "HOST_ERROR",
        }
    }
}

/// Preserves `AppError`'s own `Display` text (its variant prefix included):
/// the Lua global surfaces that text verbatim, so this must produce
/// byte-identical error strings for it.
impl From<AppError> for SpacesError {
    fn from(e: AppError) -> Self {
        let message = e.to_string();
        match e {
            AppError::NotFound(_) => Self::NotFound(message),
            AppError::Forbidden(_) => Self::NotAuthorized(message),
            AppError::Conflict(_) => Self::Conflict(message),
            AppError::BadGateway(_) => Self::Pds(message),
            AppError::BadRequest(_) => Self::BadInput(message),
            _ => Self::Other(message),
        }
    }
}

pub async fn require_enabled(state: &AppState) -> Result<(), SpacesError> {
    let enabled = crate::feature_flags::is_enabled(
        &state.db,
        crate::feature_flags::FeatureFlag::SPACES_ENABLED,
        state.db_backend,
    )
    .await;
    if enabled {
        Ok(())
    } else {
        Err(SpacesError::Disabled)
    }
}

/// The `read`/`write`/`read_self`/`none` shorthand a script sends instead of
/// the member booleans directly.
pub fn parse_access(s: &str) -> Result<MemberAccess, SpacesError> {
    MemberAccess::parse_wire(s)
        .ok_or_else(|| SpacesError::BadInput(format!("invalid access '{s}'")))
}

/// `Space` plus the `at://` URI a script needs to address it again — the one
/// field `Space`'s own serialization doesn't carry.
pub fn space_info(space: &Space) -> SpaceInfo {
    SpaceInfo {
        uri: format!(
            "at://{}/space/{}/{}",
            space.did, space.type_nsid, space.skey
        ),
        id: space.id.clone(),
        did: space.did.clone(),
        authority_did: space.authority_did.clone(),
        creator_did: space.creator_did.clone(),
        type_nsid: space.type_nsid.clone(),
        skey: space.skey.clone(),
        display_name: space.display_name.clone(),
        description: space.description.clone(),
        read_policy: serde_json::to_value(&space.read_policy).unwrap_or_default(),
        write_policy: serde_json::to_value(&space.write_policy).unwrap_or_default(),
        app_access: serde_json::to_value(&space.app_access).unwrap_or_default(),
        config: serde_json::to_value(&space.config).unwrap_or_default(),
        revision: space.revision.clone(),
        created_at: space.created_at.clone(),
        updated_at: space.updated_at.clone(),
    }
}

pub fn record_info(record: &SpaceRecord) -> SpaceRecordInfo {
    SpaceRecordInfo {
        uri: record.uri.clone(),
        collection: record.collection.clone(),
        rkey: record.rkey.clone(),
        record: record.record.clone(),
        cid: record.cid.clone(),
        author_did: record.author_did.clone(),
    }
}

pub const MAX_QUERY_LIMIT: i64 = 100;
pub const DEFAULT_QUERY_LIMIT: i64 = 50;

/// Decodes an optional wire `Value` into one of `spaces::types`'s structured
/// fields (`Policy`, `AppAccess`, `SpaceConfig`), shared by every write that
/// takes one, so a malformed or unknown-variant document is reported as
/// `BadInput` in exactly one place rather than once per caller.
fn decode<T: serde::de::DeserializeOwned>(
    value: Option<serde_json::Value>,
) -> Result<Option<T>, SpacesError> {
    value
        .map(|v| serde_json::from_value(v).map_err(|e| SpacesError::BadInput(e.to_string())))
        .transpose()
}

fn patch_to_option(patch: Patch<String>) -> Option<Option<String>> {
    match patch {
        Patch::Unchanged => None,
        Patch::Clear => Some(None),
        Patch::Set(s) => Some(Some(s)),
    }
}

pub async fn info(state: &AppState, spec: SpacesInfo) -> Result<Option<SpaceInfo>, SpacesError> {
    require_enabled(state).await?;
    match service::resolve_space(state, &spec.uri).await {
        Ok(space) => Ok(Some(space_info(&space))),
        Err(AppError::NotFound(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub async fn query(state: &AppState, spec: SpacesQuery) -> Result<SpaceRecordsPage, SpacesError> {
    require_enabled(state).await?;
    let space = service::resolve_space(state, &spec.uri).await?;
    // A non-positive limit is never something a caller actually wants, and
    // `list_space_records` binds it straight into `LIMIT ?` — on SQLite a
    // negative bind means "no limit", turning one call into an unbounded
    // read of the whole space. Refusing is cleaner than clamping it up to 1,
    // since a caller who sent 0 or a negative number almost certainly made a
    // mistake worth surfacing rather than silently reinterpreting.
    let limit = match spec.limit {
        Some(limit) if limit < 1 => {
            return Err(SpacesError::BadInput(format!(
                "limit must be at least 1, got {limit}"
            )));
        }
        Some(limit) => limit.min(MAX_QUERY_LIMIT),
        None => DEFAULT_QUERY_LIMIT,
    };
    let (records, cursor) = crate::spaces::db::list_space_records(
        &state.db,
        state.db_backend,
        &space.id,
        None,
        spec.collection.as_deref(),
        limit,
        spec.cursor.as_deref(),
        false,
    )
    .await?;
    Ok(SpaceRecordsPage {
        records: records.iter().map(record_info).collect(),
        cursor,
    })
}

pub async fn members(
    state: &AppState,
    spec: SpacesMembers,
) -> Result<Vec<SpaceMemberInfo>, SpacesError> {
    require_enabled(state).await?;
    let space = service::resolve_space(state, &spec.uri).await?;
    let resolved =
        crate::spaces::members::resolve_members(&state.db, state.db_backend, &space.id).await?;
    Ok(resolved
        .into_iter()
        .map(|m| SpaceMemberInfo {
            did: m.did,
            access: m.access.as_wire_str().to_string(),
        })
        .collect())
}

/// `None` covers both "no such space" and "not a member" — a script asking
/// for a DID's access has no need to tell those apart, and folding them
/// together is what lets this skip `resolve_space`'s `NotFound` entirely.
pub async fn access(state: &AppState, spec: SpacesAccess) -> Result<Option<String>, SpacesError> {
    require_enabled(state).await?;
    let uri = crate::spaces::SpaceUri::parse(&spec.uri)?;
    let space = crate::spaces::db::get_space_by_address(
        &state.db,
        state.db_backend,
        &uri.did,
        &uri.type_nsid,
        &uri.skey,
    )
    .await?;
    let Some(space) = space else {
        return Ok(None);
    };
    let access =
        crate::spaces::members::is_member(&state.db, state.db_backend, &space.id, &spec.did)
            .await?;
    Ok(access.map(|a| a.as_wire_str().to_string()))
}

pub async fn create(
    state: &AppState,
    caller_did: &str,
    spec: SpacesCreate,
) -> Result<SpaceInfo, SpacesError> {
    require_enabled(state).await?;
    let read_policy = decode::<Policy>(spec.read_policy)?;
    let write_policy = decode::<Policy>(spec.write_policy)?;
    let app_access = decode::<AppAccess>(spec.app_access)?;
    let config = decode::<SpaceConfig>(spec.config)?;
    let space = service::create_space(
        state,
        caller_did,
        &spec.type_nsid,
        &spec.skey,
        spec.display_name,
        spec.description,
        read_policy,
        write_policy,
        app_access,
        config,
    )
    .await?;
    Ok(space_info(&space))
}

pub async fn accept_invite(
    state: &AppState,
    caller_did: &str,
    spec: SpacesAcceptInvite,
) -> Result<SpaceInfo, SpacesError> {
    require_enabled(state).await?;
    let (space_uri, _access) = service::accept_invite(state, caller_did, &spec.token).await?;
    let space = service::resolve_space(state, &space_uri).await?;
    Ok(space_info(&space))
}

pub async fn write_record(
    state: &AppState,
    caller_did: &str,
    spec: SpaceRecordWrite,
) -> Result<RecordRef, SpacesError> {
    require_enabled(state).await?;
    let (uri, cid) = service::create_record(
        state,
        caller_did,
        None,
        &spec.uri,
        &spec.collection,
        spec.record,
    )
    .await?;
    Ok(RecordRef { uri, cid })
}

pub async fn put_record(
    state: &AppState,
    caller_did: &str,
    spec: SpaceRecordPut,
) -> Result<RecordRef, SpacesError> {
    require_enabled(state).await?;
    let (uri, cid) = service::put_record(
        state,
        caller_did,
        None,
        &spec.uri,
        &spec.collection,
        &spec.rkey,
        spec.record,
        spec.swap_cid,
    )
    .await?;
    Ok(RecordRef { uri, cid })
}

pub async fn delete_record(
    state: &AppState,
    caller_did: &str,
    spec: SpaceRecordDelete,
) -> Result<(), SpacesError> {
    require_enabled(state).await?;
    service::delete_record(
        state,
        caller_did,
        &spec.uri,
        &spec.collection,
        &spec.rkey,
        spec.swap_cid,
    )
    .await?;
    Ok(())
}

pub async fn add_member(
    state: &AppState,
    caller_did: &str,
    spec: SpaceMemberAdd,
) -> Result<SpaceMemberInfo, SpacesError> {
    require_enabled(state).await?;
    let access = spec.access.as_deref().map(parse_access).transpose()?;
    let member = service::add_member(
        state,
        caller_did,
        &spec.uri,
        &spec.did,
        access,
        spec.is_delegation,
    )
    .await?;
    Ok(SpaceMemberInfo {
        did: member.did,
        access: member.access.as_wire_str().to_string(),
    })
}

/// The upsert form of `add_member`: a repeat call updates rather than
/// conflicts, and `service::put_member` preserves an existing member's
/// `read_self`. An absent `access` defaults to read, same as `add_member`.
pub async fn set_member(
    state: &AppState,
    caller_did: &str,
    spec: SpaceMemberAdd,
) -> Result<SpaceMemberInfo, SpacesError> {
    require_enabled(state).await?;
    let access = match spec.access.as_deref() {
        Some(s) => parse_access(s)?,
        None => MemberAccess::READ,
    };
    let member = service::put_member(
        state,
        caller_did,
        &spec.uri,
        &spec.did,
        access,
        spec.is_delegation,
    )
    .await?;
    Ok(SpaceMemberInfo {
        did: member.did,
        access: member.access.as_wire_str().to_string(),
    })
}

pub async fn remove_member(
    state: &AppState,
    caller_did: &str,
    spec: SpaceMemberRemove,
) -> Result<(), SpacesError> {
    require_enabled(state).await?;
    service::remove_member(state, caller_did, &spec.uri, &spec.did).await?;
    Ok(())
}

/// `display_name`/`description` are [`Patch`]es: `Unchanged` leaves the
/// field alone, `Clear` sets it to `NULL`, `Set` replaces it — the one place
/// that three-way rule is applied, so the global and the import agree on it
/// by construction. The policy fields stay whole replacements, since a
/// script always sends a complete document.
pub async fn update(
    state: &AppState,
    caller_did: &str,
    spec: SpaceUpdate,
) -> Result<SpaceInfo, SpacesError> {
    require_enabled(state).await?;
    let display_name = patch_to_option(spec.display_name);
    let description = patch_to_option(spec.description);
    let read_policy = decode::<Policy>(spec.read_policy)?;
    let write_policy = decode::<Policy>(spec.write_policy)?;
    let app_access = decode::<AppAccess>(spec.app_access)?;
    let config = decode::<SpaceConfig>(spec.config)?;
    let space = service::update_space(
        state,
        caller_did,
        &spec.uri,
        display_name,
        description,
        read_policy,
        write_policy,
        app_access,
        config,
    )
    .await?;
    Ok(space_info(&space))
}

pub async fn delete(
    state: &AppState,
    caller_did: &str,
    spec: happyview_plugin_sdk::wire::SpaceDelete,
) -> Result<(), SpacesError> {
    require_enabled(state).await?;
    service::delete_space(state, caller_did, &spec.uri).await?;
    Ok(())
}

pub async fn create_invite(
    state: &AppState,
    caller_did: &str,
    spec: happyview_plugin_sdk::wire::SpaceInviteCreate,
) -> Result<SpaceInviteInfo, SpacesError> {
    require_enabled(state).await?;
    let access = spec.access.as_deref().map(parse_access).transpose()?;
    let (invite, token) = service::create_invite(
        state,
        caller_did,
        &spec.uri,
        access,
        spec.max_uses,
        spec.expires_at,
    )
    .await?;
    Ok(SpaceInviteInfo {
        invite_id: invite.id,
        token,
        access: invite.access.as_wire_str().to_string(),
        max_uses: invite.max_uses,
        expires_at: invite.expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use crate::config::Config;
    use crate::db::DatabaseBackend;
    use crate::lexicon::LexiconRegistry;
    use serial_test::serial;
    use tokio::sync::watch;

    macro_rules! require_test_db {
        () => {
            if std::env::var("TEST_DATABASE_URL").is_err() {
                eprintln!("skipped (TEST_DATABASE_URL not set)");
                return;
            }
        };
    }

    /// Mirrors `lua::spaces_api`'s `db_test_state`: a migrated Postgres
    /// `AppState` (`TEST_DATABASE_URL`), the same shape the Lua global's own
    /// tests build against, so the two suites see identical behaviour from
    /// identical setups.
    async fn db_test_state() -> AppState {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must be set for host::spaces integration tests");
        let backend = DatabaseBackend::from_url(&url);
        let pool = crate::db::connect(&url, backend).await;

        let config = Config {
            host: "127.0.0.1".into(),
            port: 0,
            database_url: String::new(),
            database_backend: backend,
            sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
            public_url: String::new(),
            user_agent: String::new(),
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
            // Spaces sign every commit with the `#atproto_space` key, which
            // is stored encrypted, so space writes need this set.
            token_encryption_key: Some(crate::test_support::TEST_ENCRYPTION_KEY),
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
            telemetry_collector_url: String::new(),
        };
        let (collections_tx, _) = watch::channel(vec![]);
        let (labeler_subscriptions_tx, _) = watch::channel(());

        let atrium_http = std::sync::Arc::new(crate::http_retry::HappyViewHttpClient::default());
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
            http_client: crate::http_retry::HappyViewHttpClient::default(),
        })
        .expect("Failed to create test OAuth client");

        let state = AppState {
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
            linked_repos_client: std::sync::Arc::new(
                crate::linked_repos::client::build(
                    "https://plc.directory",
                    "http://127.0.0.1:0/oauth-client-metadata.json",
                    "http://127.0.0.1:0",
                    "http://127.0.0.1:0/auth/callback".into(),
                    true,
                    vec![atrium_oauth::Scope::Known(
                        atrium_oauth::KnownScope::Atproto,
                    )],
                    crate::auth::oauth_store::DbStateStore::new(pool.clone(), backend),
                    pool.clone(),
                    backend,
                    None,
                )
                .expect("Failed to create test linked-repo OAuth client"),
            ),
            linked_repos_client_kid: None,
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
            client_jwks: Vec::new(),
            telemetry_counters: std::sync::Arc::new(crate::telemetry::counters::Counters::new()),
        };

        crate::test_support::provision_space_signing_key(&state).await;

        state
    }

    /// Idempotent upsert into `happyview_instance_settings`. The flag row is
    /// global to the whole `TEST_DATABASE_URL` database, so every test that
    /// relies on it enabled runs `#[serial(spaces_feature_flag)]` alongside
    /// the one test that turns it off.
    async fn enable_spaces_feature(state: &AppState) {
        let sql = crate::db::adapt_sql(
            "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT (key) DO UPDATE SET value = ?, updated_at = ?",
            state.db_backend,
        );
        let now = crate::db::now_rfc3339();
        crate::db::query(&sql)
            .bind(crate::feature_flags::FeatureFlag::SPACES_ENABLED)
            .bind("true")
            .bind(&now)
            .bind("true")
            .bind(&now)
            .execute(&state.db)
            .await
            .expect("failed to enable spaces_enabled feature flag");
    }

    async fn disable_spaces_feature(state: &AppState) {
        let sql = crate::db::adapt_sql(
            "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT (key) DO UPDATE SET value = ?, updated_at = ?",
            state.db_backend,
        );
        let now = crate::db::now_rfc3339();
        crate::db::query(&sql)
            .bind(crate::feature_flags::FeatureFlag::SPACES_ENABLED)
            .bind("false")
            .bind(&now)
            .bind("false")
            .bind(&now)
            .execute(&state.db)
            .await
            .expect("failed to disable spaces_enabled feature flag");
    }

    /// A migrated Postgres `AppState`, the flag on, and one space with the
    /// given DID already a write member. Randomised DIDs/skeys so parallel
    /// tests sharing `TEST_DATABASE_URL` don't collide.
    async fn seeded_space() -> (AppState, String, String) {
        let state = db_test_state().await;
        enable_spaces_feature(&state).await;

        let unique = uuid::Uuid::new_v4().simple().to_string();
        let space_id = uuid::Uuid::new_v4().to_string();
        let member_did = format!("did:plc:hostwriter{unique}");
        let space_did = format!("did:plc:hostowner{unique}");
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
            read_policy: crate::spaces::types::Policy::MemberList,
            write_policy: crate::spaces::types::Policy::MemberList,
            app_access: AppAccess::default(),
            config: SpaceConfig::default(),
            revision: None,
            created_at: crate::db::now_rfc3339(),
            updated_at: crate::db::now_rfc3339(),
        };
        crate::spaces::db::create_space(&state.db, state.db_backend, &space)
            .await
            .expect("failed to seed test space");

        let member = crate::spaces::types::SpaceMember {
            id: uuid::Uuid::new_v4().to_string(),
            space_id: space_id.clone(),
            did: member_did.clone(),
            access: MemberAccess::WRITE,
            is_delegation: false,
            granted_by: None,
            created_at: crate::db::now_rfc3339(),
        };
        crate::spaces::db::add_member(&state.db, state.db_backend, &member)
            .await
            .expect("failed to seed test member");

        let space_uri = format!("at://{space_did}/space/{type_nsid}/{skey}");
        (state, space_uri, member_did)
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn create_then_info_round_trips_every_field() {
        require_test_db!();
        let state = db_test_state().await;
        enable_spaces_feature(&state).await;
        let unique = uuid::Uuid::new_v4().simple().to_string();
        let did = format!("did:plc:creator{unique}");

        let created = create(
            &state,
            &did,
            SpacesCreate {
                type_nsid: "com.example.forum".into(),
                skey: format!("main{unique}"),
                display_name: Some("Main forum".into()),
                description: Some("The default forum".into()),
                read_policy: None,
                write_policy: None,
                app_access: None,
                config: None,
            },
        )
        .await
        .expect("create should succeed");
        assert_eq!(created.display_name.as_deref(), Some("Main forum"));
        assert_eq!(created.description.as_deref(), Some("The default forum"));
        assert_eq!(created.did, did);
        assert_eq!(created.authority_did, did);
        assert_eq!(created.creator_did, did);

        let fetched = info(
            &state,
            SpacesInfo {
                uri: created.uri.clone(),
            },
        )
        .await
        .expect("info should succeed")
        .expect("space should exist");
        // `db::create_space` stamps its own `created_at`/`updated_at` at
        // insert time rather than trusting the caller's `Space` value, so
        // only a DB round trip's timestamps are meaningful to compare.
        assert_eq!(
            SpaceInfo {
                created_at: fetched.created_at.clone(),
                updated_at: fetched.updated_at.clone(),
                ..created
            },
            fetched
        );
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn info_on_an_unknown_space_is_none() {
        require_test_db!();
        let state = db_test_state().await;
        enable_spaces_feature(&state).await;

        let fetched = info(
            &state,
            SpacesInfo {
                uri: "at://did:plc:nobody/space/com.example.forum/main".into(),
            },
        )
        .await
        .expect("info should succeed on an unknown space");
        assert!(fetched.is_none());
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn query_clamps_limit_and_omits_cursor_on_the_last_page() {
        require_test_db!();
        let (state, space_uri, member_did) = seeded_space().await;

        for i in 0..3 {
            write_record(
                &state,
                &member_did,
                SpaceRecordWrite {
                    uri: space_uri.clone(),
                    collection: "com.example.item".into(),
                    record: serde_json::json!({"n": i}),
                },
            )
            .await
            .expect("write_record should succeed");
        }

        let page = query(
            &state,
            SpacesQuery {
                uri: space_uri.clone(),
                collection: None,
                limit: Some(1000),
                cursor: None,
            },
        )
        .await
        .expect("query should succeed");
        assert_eq!(
            page.records.len(),
            3,
            "limit should clamp to MAX_QUERY_LIMIT, not truncate the page"
        );
        assert!(page.cursor.is_none(), "the last page carries no cursor");
        assert!(page.records.iter().all(|r| r.author_did == member_did));
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn query_refuses_a_non_positive_limit_before_touching_the_database() {
        require_test_db!();
        let (state, space_uri, _member_did) = seeded_space().await;

        for limit in [0, -1] {
            let err = query(
                &state,
                SpacesQuery {
                    uri: space_uri.clone(),
                    collection: None,
                    limit: Some(limit),
                    cursor: None,
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(err, SpacesError::BadInput(_)), "{limit}: {err}");
            assert_eq!(err.code(), "BAD_INPUT");
        }
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn access_is_none_for_a_non_member_and_an_unknown_space() {
        require_test_db!();
        let (state, space_uri, _member_did) = seeded_space().await;

        let non_member = access(
            &state,
            SpacesAccess {
                uri: space_uri.clone(),
                did: "did:plc:stranger".into(),
            },
        )
        .await
        .expect("access should succeed for a non-member");
        assert!(non_member.is_none());

        let unknown_space = access(
            &state,
            SpacesAccess {
                uri: "at://did:plc:nobody/space/com.example.forum/main".into(),
                did: "did:plc:stranger".into(),
            },
        )
        .await
        .expect("access should succeed for an unknown space");
        assert!(unknown_space.is_none());
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn write_record_by_a_non_member_is_not_authorized() {
        require_test_db!();
        let (state, space_uri, _member_did) = seeded_space().await;

        let err = write_record(
            &state,
            "did:plc:stranger",
            SpaceRecordWrite {
                uri: space_uri,
                collection: "com.example.item".into(),
                record: serde_json::json!({"text": "hi"}),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SpacesError::NotAuthorized(_)), "{err}");
        assert_eq!(err.code(), "NOT_AUTHORIZED");
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn add_member_twice_conflicts_while_set_member_twice_preserves_read_self() {
        require_test_db!();
        let (state, space_uri, admin_did) = seeded_space().await;
        let target = "did:plc:target";

        add_member(
            &state,
            &admin_did,
            SpaceMemberAdd {
                uri: space_uri.clone(),
                did: target.into(),
                access: Some("read_self".into()),
                is_delegation: None,
            },
        )
        .await
        .expect("first add_member should succeed");

        let err = add_member(
            &state,
            &admin_did,
            SpaceMemberAdd {
                uri: space_uri.clone(),
                did: target.into(),
                access: Some("read".into()),
                is_delegation: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SpacesError::Conflict(_)), "{err}");
        assert_eq!(err.code(), "CONFLICT");

        // `set_member` on the same DID is an upsert, not a conflict, and
        // preserves the `read_self` the member already had even though this
        // call's `access` doesn't ask for it. Sending "read" rather than
        // "write" is deliberate: `as_wire_str` reports "write" whenever
        // `write` is set regardless of `read_self`, so asserting against a
        // "write" result can't distinguish preservation from
        // `add_member`-style replacement (which would reset `read_self` to
        // false). "read" leaves `write` false, so the wire string can only
        // read "read_self" if the bit survived, and "read" if it didn't.
        let updated = set_member(
            &state,
            &admin_did,
            SpaceMemberAdd {
                uri: space_uri.clone(),
                did: target.into(),
                access: Some("read".into()),
                is_delegation: None,
            },
        )
        .await
        .expect("set_member should succeed on an existing member");
        assert_eq!(updated.access, "read_self");

        let access_after = access(
            &state,
            SpacesAccess {
                uri: space_uri,
                did: target.into(),
            },
        )
        .await
        .expect("access should succeed")
        .expect("target should still be a member");
        assert_eq!(access_after, "read_self");
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn update_clears_display_name_on_false_and_leaves_it_alone_when_absent() {
        require_test_db!();
        let (state, space_uri, admin_did) = seeded_space().await;

        update(
            &state,
            &admin_did,
            SpaceUpdate {
                uri: space_uri.clone(),
                display_name: Patch::Set("Named".into()),
                description: Patch::Unchanged,
                read_policy: None,
                write_policy: None,
                app_access: None,
                config: None,
            },
        )
        .await
        .expect("update should set display_name");

        let unchanged = update(
            &state,
            &admin_did,
            SpaceUpdate {
                uri: space_uri.clone(),
                display_name: Patch::Unchanged,
                description: Patch::Unchanged,
                read_policy: None,
                write_policy: None,
                app_access: None,
                config: None,
            },
        )
        .await
        .expect("update with nothing set should succeed");
        assert_eq!(unchanged.display_name.as_deref(), Some("Named"));

        let cleared = update(
            &state,
            &admin_did,
            SpaceUpdate {
                uri: space_uri,
                display_name: Patch::Clear,
                description: Patch::Unchanged,
                read_policy: None,
                write_policy: None,
                app_access: None,
                config: None,
            },
        )
        .await
        .expect("update should clear display_name");
        assert!(cleared.display_name.is_none());
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn create_invite_then_accept_invite_makes_the_joiner_a_member() {
        require_test_db!();
        let (state, space_uri, admin_did) = seeded_space().await;
        let joiner = format!("did:plc:joiner{}", uuid::Uuid::new_v4().simple());

        let invite = create_invite(
            &state,
            &admin_did,
            happyview_plugin_sdk::wire::SpaceInviteCreate {
                uri: space_uri.clone(),
                access: Some("write".into()),
                max_uses: None,
                expires_at: None,
            },
        )
        .await
        .expect("create_invite should succeed");
        assert_eq!(invite.access, "write");

        let joined = accept_invite(
            &state,
            &joiner,
            SpacesAcceptInvite {
                token: invite.token,
            },
        )
        .await
        .expect("accept_invite should succeed");
        assert_eq!(joined.uri, space_uri);

        let joiner_access = access(
            &state,
            SpacesAccess {
                uri: space_uri,
                did: joiner,
            },
        )
        .await
        .expect("access should succeed")
        .expect("joiner should now be a member");
        assert_eq!(joiner_access, "write");
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn feature_flag_off_disables_every_operation() {
        require_test_db!();
        let state = db_test_state().await;
        disable_spaces_feature(&state).await;
        let uri = "at://did:plc:x/space/com.example.forum/main".to_string();

        let err = info(&state, SpacesInfo { uri: uri.clone() })
            .await
            .unwrap_err();
        assert!(matches!(err, SpacesError::Disabled), "{err}");
        assert_eq!(err.code(), "SPACES_DISABLED");

        // Every function opens with `require_enabled`, so a second read and
        // a write both refuse the same way — before either ever resolves the
        // (nonexistent) space or asks for a caller.
        let err = query(
            &state,
            SpacesQuery {
                uri: uri.clone(),
                collection: None,
                limit: None,
                cursor: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SpacesError::Disabled), "{err}");
        assert_eq!(err.code(), "SPACES_DISABLED");

        let err = write_record(
            &state,
            "did:plc:x",
            SpaceRecordWrite {
                uri,
                collection: "com.example.item".into(),
                record: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SpacesError::Disabled), "{err}");
        assert_eq!(err.code(), "SPACES_DISABLED");
    }
}
