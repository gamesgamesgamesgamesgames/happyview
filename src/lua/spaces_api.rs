use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
use std::sync::Arc;

use crate::AppState;
use crate::spaces::{SpaceUri, service};

pub const SPACES_NO_CALLER_MSG: &str =
    "this space operation requires an authenticated caller (caller_did is not set in this context)";

async fn spaces_enabled(state: &AppState) -> bool {
    crate::feature_flags::is_enabled(
        &state.db,
        crate::feature_flags::FeatureFlag::SPACES_ENABLED,
        state.db_backend,
    )
    .await
}

pub(crate) struct LuaSpace {
    state: Arc<AppState>,
    /// 4-segment space URI: at://{did}/space/{type}/{skey}
    space_uri: String,
    caller_did: Option<String>,
}

impl LuaSpace {
    fn require_caller(&self) -> LuaResult<String> {
        self.caller_did
            .clone()
            .ok_or_else(|| mlua::Error::runtime(SPACES_NO_CALLER_MSG))
    }
}

impl mlua::UserData for LuaSpace {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("uri", |_, this| Ok(this.space_uri.clone()));
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // space:write_record{ collection, record } -> { uri, cid }
        methods.add_async_method("write_record", |lua, this, opts: mlua::Table| async move {
            let state = this.state.clone();
            let space_uri = this.space_uri.clone();
            let did = this.require_caller()?;
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            let collection: String = opts
                .get("collection")
                .map_err(|_| mlua::Error::runtime("write_record: collection is required"))?;
            let record_val: mlua::Value = opts.get("record")?;
            if record_val.is_nil() {
                return Err(mlua::Error::runtime("write_record: record is required"));
            }
            let record: serde_json::Value = lua.from_value(record_val)?;
            let (uri, cid) =
                service::create_record(&state, &did, None, &space_uri, &collection, record)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("write_record: {e}")))?;
            let result = lua.create_table()?;
            result.set("uri", uri)?;
            result.set("cid", cid)?;
            Ok(mlua::Value::Table(result))
        });

        // space:put_record{ collection, rkey, record, swap_cid? } -> { uri, cid }
        methods.add_async_method("put_record", |lua, this, opts: mlua::Table| async move {
            let state = this.state.clone();
            let space_uri = this.space_uri.clone();
            let did = this.require_caller()?;
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            let collection: String = opts
                .get("collection")
                .map_err(|_| mlua::Error::runtime("put_record: collection is required"))?;
            let rkey: String = opts
                .get("rkey")
                .map_err(|_| mlua::Error::runtime("put_record: rkey is required"))?;
            let record_val: mlua::Value = opts.get("record")?;
            if record_val.is_nil() {
                return Err(mlua::Error::runtime("put_record: record is required"));
            }
            let record: serde_json::Value = lua.from_value(record_val)?;
            let swap_cid: Option<String> = opts.get("swap_cid").ok();
            let (uri, cid) = service::put_record(
                &state,
                &did,
                None,
                &space_uri,
                &collection,
                &rkey,
                record,
                swap_cid,
            )
            .await
            .map_err(|e| mlua::Error::runtime(format!("put_record: {e}")))?;
            let result = lua.create_table()?;
            result.set("uri", uri)?;
            result.set("cid", cid)?;
            Ok(mlua::Value::Table(result))
        });

        // space:delete_record{ collection, rkey, swap_cid? } -> true
        methods.add_async_method(
            "delete_record",
            |_lua, this, opts: mlua::Table| async move {
                let state = this.state.clone();
                let space_uri = this.space_uri.clone();
                let did = this.require_caller()?;
                if !spaces_enabled(&state).await {
                    return Err(mlua::Error::runtime("spaces feature is not enabled"));
                }
                let collection: String = opts
                    .get("collection")
                    .map_err(|_| mlua::Error::runtime("delete_record: collection is required"))?;
                let rkey: String = opts
                    .get("rkey")
                    .map_err(|_| mlua::Error::runtime("delete_record: rkey is required"))?;
                let swap_cid: Option<String> = opts.get("swap_cid").ok();
                service::delete_record(&state, &did, &space_uri, &collection, &rkey, swap_cid)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("delete_record: {e}")))?;
                Ok(true)
            },
        );

        // space:add_member{ did, access?, is_delegation? } -> { did, access }
        methods.add_async_method("add_member", |lua, this, opts: mlua::Table| async move {
            let state = this.state.clone();
            let space_uri = this.space_uri.clone();
            let actor = this.require_caller()?;
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            let member_did: String = opts
                .get("did")
                .map_err(|_| mlua::Error::runtime("add_member: did is required"))?;
            let access: Option<crate::spaces::types::SpaceAccess> =
                match opts.get::<Option<String>>("access").ok().flatten() {
                    Some(s) => {
                        Some(crate::spaces::types::SpaceAccess::parse(&s).ok_or_else(|| {
                            mlua::Error::runtime(format!("add_member: invalid access '{s}'"))
                        })?)
                    }
                    None => None,
                };
            let is_delegation: Option<bool> = opts.get("is_delegation").ok();
            let member = service::add_member(
                &state,
                &actor,
                &space_uri,
                &member_did,
                access,
                is_delegation,
            )
            .await
            .map_err(|e| mlua::Error::runtime(format!("add_member: {e}")))?;
            let result = lua.create_table()?;
            result.set("did", member.did)?;
            result.set("access", member.access.as_str())?;
            Ok(mlua::Value::Table(result))
        });

        // space:remove_member{ did } -> true
        methods.add_async_method(
            "remove_member",
            |_lua, this, opts: mlua::Table| async move {
                let state = this.state.clone();
                let space_uri = this.space_uri.clone();
                let actor = this.require_caller()?;
                if !spaces_enabled(&state).await {
                    return Err(mlua::Error::runtime("spaces feature is not enabled"));
                }
                let member_did: String = opts
                    .get("did")
                    .map_err(|_| mlua::Error::runtime("remove_member: did is required"))?;
                service::remove_member(&state, &actor, &space_uri, &member_did)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("remove_member: {e}")))?;
                Ok(true)
            },
        );

        // space:members() -> [{ did, access }]
        methods.add_async_method("members", |lua, this, ()| async move {
            let state = this.state.clone();
            let space_uri = this.space_uri.clone();
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            let space = service::resolve_space(&state, &space_uri)
                .await
                .map_err(|e| mlua::Error::runtime(format!("members: {e}")))?;
            let members =
                crate::spaces::members::resolve_members(&state.db, state.db_backend, &space.id)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("members: {e}")))?;
            let result = lua.create_table()?;
            for (i, m) in members.iter().enumerate() {
                let entry = lua.create_table()?;
                entry.set("did", m.did.as_str())?;
                entry.set("access", m.access.as_str())?;
                result.set(i + 1, entry)?;
            }
            Ok(mlua::Value::Table(result))
        });

        // space:is_member(did) -> bool ; space:access(did) -> 'read'|'write'|nil
        methods.add_async_method("is_member", |_lua, this, did: String| async move {
            let state = this.state.clone();
            let space_uri = this.space_uri.clone();
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            let space = service::resolve_space(&state, &space_uri)
                .await
                .map_err(|e| mlua::Error::runtime(format!("is_member: {e}")))?;
            let access =
                crate::spaces::members::is_member(&state.db, state.db_backend, &space.id, &did)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("is_member: {e}")))?;
            Ok(access.is_some())
        });
        methods.add_async_method("access", |_lua, this, did: String| async move {
            let state = this.state.clone();
            let space_uri = this.space_uri.clone();
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            let space = service::resolve_space(&state, &space_uri)
                .await
                .map_err(|e| mlua::Error::runtime(format!("access: {e}")))?;
            let access =
                crate::spaces::members::is_member(&state.db, state.db_backend, &space.id, &did)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("access: {e}")))?;
            Ok(access.map(|a| a.as_str().to_string()))
        });

        // space:update{ display_name?, description?, mint_policy?, app_access?, managing_app_did?, config? } -> true
        // For nullable patch fields, a Lua `false` clears (sets to nil); omitting leaves unchanged.
        methods.add_async_method("update", |lua, this, opts: mlua::Table| async move {
            let state = this.state.clone();
            let space_uri = this.space_uri.clone();
            let actor = this.require_caller()?;
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            // Patch semantics: key present with string => Some(Some(s)); key present and false => Some(None) (clear); key absent => None.
            fn nullable(opts: &mlua::Table, key: &str) -> mlua::Result<Option<Option<String>>> {
                match opts.get::<mlua::Value>(key)? {
                    mlua::Value::Nil => Ok(None),
                    mlua::Value::Boolean(false) => Ok(Some(None)),
                    mlua::Value::String(s) => Ok(Some(Some(s.to_str()?.to_owned()))),
                    _ => Err(mlua::Error::runtime(format!(
                        "update: {key} must be a string or false"
                    ))),
                }
            }
            let display_name = nullable(&opts, "display_name")?;
            let description = nullable(&opts, "description")?;
            let managing_app_did = nullable(&opts, "managing_app_did")?;
            let mint_policy: Option<crate::spaces::types::MintPolicy> =
                match opts.get::<Option<String>>("mint_policy").ok().flatten() {
                    Some(s) => {
                        Some(crate::spaces::types::MintPolicy::parse(&s).ok_or_else(|| {
                            mlua::Error::runtime(format!("update: invalid mint_policy '{s}'"))
                        })?)
                    }
                    None => None,
                };
            let app_access: Option<crate::spaces::types::AppAccess> =
                match opts.get::<mlua::Value>("app_access") {
                    Ok(mlua::Value::Nil) | Err(_) => None,
                    Ok(v) => Some(lua.from_value(v)?),
                };
            let config: Option<crate::spaces::types::SpaceConfig> =
                match opts.get::<mlua::Value>("config") {
                    Ok(mlua::Value::Nil) | Err(_) => None,
                    Ok(v) => Some(lua.from_value(v)?),
                };
            service::update_space(
                &state,
                &actor,
                &space_uri,
                display_name,
                description,
                mint_policy,
                app_access,
                managing_app_did,
                config,
            )
            .await
            .map_err(|e| mlua::Error::runtime(format!("update: {e}")))?;
            Ok(true)
        });

        // space:delete() -> true
        methods.add_async_method("delete", |_lua, this, ()| async move {
            let state = this.state.clone();
            let space_uri = this.space_uri.clone();
            let actor = this.require_caller()?;
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            service::delete_space(&state, &actor, &space_uri)
                .await
                .map_err(|e| mlua::Error::runtime(format!("delete: {e}")))?;
            Ok(true)
        });

        // space:query{ collection?, limit?, cursor? } -> { records, cursor }
        methods.add_async_method("query", |lua, this, opts: mlua::Table| async move {
            let state = this.state.clone();
            let space_uri = this.space_uri.clone();
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            let space = service::resolve_space(&state, &space_uri)
                .await
                .map_err(|e| mlua::Error::runtime(format!("query: {e}")))?;
            let collection: Option<String> = opts.get("collection").ok();
            let limit: i64 = opts.get("limit").unwrap_or(50);
            let cursor: Option<String> = opts.get("cursor").ok();
            let (records, next_cursor) = crate::spaces::db::list_space_records(
                &state.db,
                state.db_backend,
                &space.id,
                None,
                collection.as_deref(),
                limit.min(100),
                cursor.as_deref(),
                false,
            )
            .await
            .map_err(|e| mlua::Error::runtime(format!("query: {e}")))?;
            let result = lua.create_table()?;
            let records_table = lua.create_table()?;
            for (i, r) in records.iter().enumerate() {
                let entry = lua.to_value(&serde_json::json!({
                    "uri": r.uri, "collection": r.collection, "rkey": r.rkey,
                    "record": r.record, "cid": r.cid, "authorDid": r.author_did,
                }))?;
                records_table.set(i + 1, entry)?;
            }
            result.set("records", records_table)?;
            match next_cursor {
                Some(c) => result.set("cursor", c)?,
                None => result.set("cursor", mlua::Value::Nil)?,
            }
            Ok(mlua::Value::Table(result))
        });

        // space:create_invite{ access?, max_uses?, expires_at? } -> { invite_id, token, access, max_uses, expires_at }
        methods.add_async_method("create_invite", |lua, this, opts: mlua::Table| async move {
            let state = this.state.clone();
            let space_uri = this.space_uri.clone();
            let actor = this.require_caller()?;
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            let access: Option<crate::spaces::types::SpaceAccess> =
                match opts.get::<Option<String>>("access").ok().flatten() {
                    Some(s) => {
                        Some(crate::spaces::types::SpaceAccess::parse(&s).ok_or_else(|| {
                            mlua::Error::runtime(format!("create_invite: invalid access '{s}'"))
                        })?)
                    }
                    None => None,
                };
            let max_uses: Option<i64> = opts.get("max_uses").ok();
            let expires_at: Option<String> = opts.get("expires_at").ok();
            let (invite, token) =
                service::create_invite(&state, &actor, &space_uri, access, max_uses, expires_at)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("create_invite: {e}")))?;
            let result = lua.create_table()?;
            result.set("invite_id", invite.id)?;
            result.set("token", token)?;
            result.set("access", invite.access.as_str())?;
            result.set("max_uses", invite.max_uses)?;
            result.set("expires_at", invite.expires_at)?;
            Ok(mlua::Value::Table(result))
        });
    }
}

/// Attach write/handle methods to the already-registered `atproto.spaces` table.
/// Must be called AFTER `register_atproto_api` (which creates `atproto.spaces`).
pub fn register_spaces_write_api(
    lua: &Lua,
    state: Arc<AppState>,
    caller_did: Option<&str>,
) -> LuaResult<()> {
    let atproto: mlua::Table = lua.globals().get("atproto")?;
    let spaces: mlua::Table = atproto.get("spaces")?;
    let caller_did = caller_did.map(|s| s.to_string());

    // atproto.spaces.get(space_uri) -> LuaSpace | nil
    let state_clone = state.clone();
    let caller_clone = caller_did.clone();
    let get_fn = lua.create_async_function(move |lua, space_uri: String| {
        let state = state_clone.clone();
        let caller_did = caller_clone.clone();
        async move {
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            let parsed = SpaceUri::parse(&space_uri)
                .map_err(|e| mlua::Error::runtime(format!("invalid space URI: {e}")))?;
            let space = service::resolve_space(&state, &parsed.space_uri()).await;
            match space {
                Ok(s) => {
                    let handle = LuaSpace {
                        state: state.clone(),
                        space_uri: format!("at://{}/space/{}/{}", s.did, s.type_nsid, s.skey),
                        caller_did,
                    };
                    Ok(mlua::Value::UserData(lua.create_userdata(handle)?))
                }
                Err(crate::error::AppError::NotFound(_)) => Ok(mlua::Value::Nil),
                Err(e) => Err(mlua::Error::runtime(format!("space lookup failed: {e}"))),
            }
        }
    })?;
    spaces.set("get", get_fn)?;

    // atproto.spaces.create{ type, skey, display_name?, description?, mint_policy?, app_access?, managing_app_did?, config? } -> LuaSpace
    let state_clone = state.clone();
    let caller_clone = caller_did.clone();
    let create_fn = lua.create_async_function(move |lua, opts: mlua::Table| {
        let state = state_clone.clone();
        let caller_did = caller_clone.clone();
        async move {
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            let did = caller_did.ok_or_else(|| mlua::Error::runtime(SPACES_NO_CALLER_MSG))?;
            let type_nsid: String = opts
                .get("type")
                .map_err(|_| mlua::Error::runtime("create: type is required"))?;
            let skey: String = opts
                .get("skey")
                .map_err(|_| mlua::Error::runtime("create: skey is required"))?;
            let display_name: Option<String> = opts.get("display_name").ok();
            let description: Option<String> = opts.get("description").ok();
            let mint_policy: Option<crate::spaces::types::MintPolicy> =
                match opts.get::<Option<String>>("mint_policy").ok().flatten() {
                    Some(s) => {
                        Some(crate::spaces::types::MintPolicy::parse(&s).ok_or_else(|| {
                            mlua::Error::runtime(format!("create: invalid mint_policy '{s}'"))
                        })?)
                    }
                    None => None,
                };
            let managing_app_did: Option<String> = opts.get("managing_app_did").ok();
            // app_access and config accept structured tables; deserialize via serde.
            let app_access: Option<crate::spaces::types::AppAccess> =
                match opts.get::<mlua::Value>("app_access") {
                    Ok(mlua::Value::Nil) | Err(_) => None,
                    Ok(v) => Some(lua.from_value(v)?),
                };
            let config: Option<crate::spaces::types::SpaceConfig> =
                match opts.get::<mlua::Value>("config") {
                    Ok(mlua::Value::Nil) | Err(_) => None,
                    Ok(v) => Some(lua.from_value(v)?),
                };
            let space = service::create_space(
                &state,
                &did,
                &type_nsid,
                &skey,
                display_name,
                description,
                mint_policy,
                app_access,
                managing_app_did,
                config,
            )
            .await
            .map_err(|e| mlua::Error::runtime(format!("create: {e}")))?;
            let handle = LuaSpace {
                state: state.clone(),
                space_uri: format!(
                    "at://{}/space/{}/{}",
                    space.did, space.type_nsid, space.skey
                ),
                caller_did: Some(did),
            };
            Ok(mlua::Value::UserData(lua.create_userdata(handle)?))
        }
    })?;
    spaces.set("create", create_fn)?;

    // atproto.spaces.accept_invite{ token } -> LuaSpace
    let state_clone = state.clone();
    let caller_clone = caller_did.clone();
    let accept_fn = lua.create_async_function(move |lua, opts: mlua::Table| {
        let state = state_clone.clone();
        let caller_did = caller_clone.clone();
        async move {
            if !spaces_enabled(&state).await {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
            let did = caller_did.ok_or_else(|| mlua::Error::runtime(SPACES_NO_CALLER_MSG))?;
            let token: String = opts
                .get("token")
                .map_err(|_| mlua::Error::runtime("accept_invite: token is required"))?;
            let (space_uri, _access) = service::accept_invite(&state, &did, &token)
                .await
                .map_err(|e| mlua::Error::runtime(format!("accept_invite: {e}")))?;
            let handle = LuaSpace {
                state: state.clone(),
                space_uri,
                caller_did: Some(did),
            };
            Ok(mlua::Value::UserData(lua.create_userdata(handle)?))
        }
    })?;
    spaces.set("accept_invite", accept_fn)?;

    #[cfg(test)]
    {
        // Test-only constructor that builds a handle without a DB lookup.
        let state_clone = state.clone();
        let caller_clone = caller_did.clone();
        let test_handle = lua.create_function(move |lua, space_uri: String| {
            let handle = LuaSpace {
                state: state_clone.clone(),
                space_uri,
                caller_did: caller_clone.clone(),
            };
            lua.create_userdata(handle)
        })?;
        spaces.set("__test_handle", test_handle)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::AppState;
    use crate::config::Config;
    use crate::db::DatabaseBackend;
    use crate::lexicon::LexiconRegistry;
    use serial_test::serial;
    use tokio::sync::watch;

    fn test_state_with_plc(plc_url: &str) -> AppState {
        let config = Config {
            host: "127.0.0.1".into(),
            port: 3000,
            database_url: String::new(),
            database_backend: crate::db::DatabaseBackend::Sqlite,
            sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
            public_url: String::new(),
            session_secret: "test-secret".into(),
            jetstream_url: String::new(),
            relay_url: String::new(),
            plc_url: plc_url.to_string(),
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
        let (tx, _) = watch::channel(vec![]);
        let (labeler_tx, _) = watch::channel(());
        sqlx::any::install_default_drivers();
        let test_db = sqlx::AnyPool::connect_lazy("sqlite::memory:").unwrap();
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
            state_store: crate::auth::oauth_store::DbStateStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            session_store: crate::auth::oauth_store::DbSessionStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
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
            db: test_db.clone(),
            backfill_db: test_db.clone(),
            db_backend: DatabaseBackend::Sqlite,
            domain_cache: crate::domain::DomainCache::new(),
            lexicons: LexiconRegistry::new(),
            collections_tx: tx,
            labeler_subscriptions_tx: labeler_tx,
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
            oauth_state_store: crate::auth::oauth_store::DbStateStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
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
                    crate::auth::oauth_store::DbStateStore::new(
                        test_db.clone(),
                        crate::db::DatabaseBackend::Sqlite,
                    ),
                    test_db.clone(),
                    crate::db::DatabaseBackend::Sqlite,
                )
                .expect("Failed to create test linked-repo OAuth client"),
            ),
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

    #[tokio::test]
    async fn spaces_write_api_is_registered() {
        let state = test_state_with_plc("");
        let lua = mlua::Lua::new();
        crate::lua::atproto_api::register_atproto_api(
            &lua,
            std::sync::Arc::new(state.clone()),
            Some("did:plc:x"),
        )
        .unwrap();
        super::register_spaces_write_api(&lua, std::sync::Arc::new(state), Some("did:plc:x"))
            .unwrap();
        let chunk = r#"
            return type(atproto.spaces.get) == "function"
                and type(atproto.spaces.create) == "function"
                and type(atproto.spaces.accept_invite) == "function"
        "#;
        let ok: bool = lua.load(chunk).eval_async().await.unwrap();
        assert!(ok);
    }

    #[tokio::test]
    async fn write_record_raises_without_caller() {
        let state = test_state_with_plc("");
        let lua = mlua::Lua::new();
        crate::lua::atproto_api::register_atproto_api(
            &lua,
            std::sync::Arc::new(state.clone()),
            None,
        )
        .unwrap();
        super::register_spaces_write_api(&lua, std::sync::Arc::new(state), None).unwrap();

        // Build a handle directly (get() would need a DB); force the no-caller path.
        let chunk = r#"
            local space = atproto.spaces.__test_handle("at://did:plc:x/space/com.example/general")
            local ok, err = pcall(function()
                return space:write_record{ collection = "com.example.item", record = { text = "hi" } }
            end)
            return tostring(err)
        "#;
        let err: String = lua.load(chunk).eval_async().await.unwrap();
        // `err` is a Rust String here; assert on it with Rust string methods.
        assert!(
            err.contains("caller"),
            "expected a no-caller error, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // DB-backed integration test (Postgres via TEST_DATABASE_URL).
    //
    // Mirrors the skip idiom used by `spaces::service`'s tests: when
    // `TEST_DATABASE_URL` isn't set, the test returns early instead of
    // failing, so `cargo test --lib` stays DB-free. Run for real via:
    // `TEST_DATABASE_URL=postgres://... cargo test -p happyview spaces_write_lua`
    // -----------------------------------------------------------------------
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
    async fn db_test_state() -> AppState {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must be set for spaces_api DB integration tests");
        let backend = DatabaseBackend::from_url(&url);
        let pool = crate::db::connect(&url, backend).await;

        let config = Config {
            host: "127.0.0.1".into(),
            port: 0,
            database_url: String::new(),
            database_backend: backend,
            sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
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
                )
                .expect("Failed to create test linked-repo OAuth client"),
            ),
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

    /// Ensure the `spaces_enabled` feature flag reads `true` for this DB
    /// (idempotent upsert into `happyview_instance_settings`).
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

    /// Force the `spaces_enabled` feature flag to read `false` for this DB
    /// (idempotent upsert into `happyview_instance_settings`).
    ///
    /// The flag row is a single global row shared by every test against the
    /// same `TEST_DATABASE_URL` database, so a test that needs the flag
    /// *disabled* can't just rely on the row never having been written —
    /// other tests in this module call `enable_spaces_feature` and run
    /// concurrently by default. Callers that need a disabled flag should
    /// call this explicitly and also tag the test `#[serial(spaces_feature_flag)]`
    /// alongside every test that calls `enable_spaces_feature`.
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

    /// Build a migrated Postgres-backed `AppState` seeded with one space and
    /// one write-member. Returns `(state, space_uri, member_did)`.
    ///
    /// Uses randomised DIDs/skeys so parallel tests sharing the same
    /// `TEST_DATABASE_URL` database don't collide.
    async fn db_seeded_space() -> (AppState, String, String) {
        let state = db_test_state().await;
        enable_spaces_feature(&state).await;

        let unique = uuid::Uuid::new_v4().simple().to_string();
        let space_id = uuid::Uuid::new_v4().to_string();
        let member_did = format!("did:plc:luawriter{unique}");
        let space_did = format!("did:plc:luaowner{unique}");
        let type_nsid = "com.example.forum";
        let skey = format!("main{unique}");

        let space = crate::spaces::types::Space {
            id: space_id.clone(),
            did: space_did.clone(),
            authority_did: member_did.clone(),
            creator_did: member_did.clone(),
            type_nsid: type_nsid.to_string(),
            skey: skey.clone(),
            display_name: None,
            description: None,
            mint_policy: crate::spaces::types::MintPolicy::MemberList,
            app_access: crate::spaces::types::AppAccess::default(),
            managing_app_did: None,
            config: crate::spaces::types::SpaceConfig::default(),
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
            access: crate::spaces::types::SpaceAccess::Write,
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
    async fn spaces_write_lua_write_record_then_query() {
        require_test_db!();
        let (state, space_uri, member_did) = db_seeded_space().await;

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(&lua, state_arc.clone(), Some(&member_did))
            .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&member_did)).unwrap();

        let chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                assert(space ~= nil, "space handle should not be nil")
                local result = space:write_record{{
                    collection = "com.example.item",
                    record = {{ text = "hello from lua" }},
                }}
                assert(result.uri ~= nil, "write_record should return a uri")
                assert(result.cid ~= nil, "write_record should return a cid")

                local q = atproto.spaces.query({{
                    space_uri = "{space_uri}",
                    collection = "com.example.item",
                }})
                assert(#q.records == 1, "expected exactly one record")
                return q.records[1].record.text
            "#
        );
        let text: String = lua.load(&chunk).eval_async().await.unwrap();
        assert_eq!(text, "hello from lua");
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn spaces_create_returns_handle_and_is_queryable() {
        require_test_db!();
        let state = db_test_state().await;
        enable_spaces_feature(&state).await;

        let unique = uuid::Uuid::new_v4().simple().to_string();
        let creator_did = format!("did:plc:luacreator{unique}");
        let skey = format!("general{unique}");

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(&lua, state_arc.clone(), Some(&creator_did))
            .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&creator_did)).unwrap();

        let chunk = format!(
            r#"
                local space = atproto.spaces.create{{
                    type = "com.example.chat",
                    skey = "{skey}",
                }}
                assert(space ~= nil, "create should return a handle")
                assert(space.uri ~= nil, "handle should have a uri")

                local again = atproto.spaces.get(space.uri)
                assert(again ~= nil, "created space should be queryable via get()")
                assert(again.uri == space.uri, "queried uri should match created uri")

                return space.uri
            "#
        );
        let uri: String = lua.load(&chunk).eval_async().await.unwrap();
        assert!(uri.starts_with("at://"), "unexpected uri: {uri}");
        assert!(uri.contains("/space/"), "unexpected uri: {uri}");
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn write_record_requires_record_body() {
        require_test_db!();
        let (state, space_uri, member_did) = db_seeded_space().await;

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(&lua, state_arc.clone(), Some(&member_did))
            .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&member_did)).unwrap();

        let chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                assert(space ~= nil, "space handle should not be nil")
                local ok, err = pcall(function()
                    return space:write_record{{ collection = "com.example.item" }}
                end)
                assert(not ok, "write_record should fail when record is missing")
                return tostring(err)
            "#
        );
        let err: String = lua.load(&chunk).eval_async().await.unwrap();
        assert!(
            err.contains("record"),
            "expected a missing-record error, got: {err}"
        );
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn add_member_then_new_member_can_write() {
        require_test_db!();
        let (state, space_uri, authority_did) = db_seeded_space().await;
        let new_member_did = format!("did:plc:newmember{}", uuid::Uuid::new_v4().simple());

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(
            &lua,
            state_arc.clone(),
            Some(&authority_did),
        )
        .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&authority_did)).unwrap();

        let add_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                assert(space ~= nil, "space handle should not be nil")
                local member = space:add_member{{ did = "{new_member_did}", access = "write" }}
                assert(member.did == "{new_member_did}", "unexpected member did")
                assert(member.access == "write", "unexpected member access")
            "#
        );
        lua.load(&add_chunk).exec_async().await.unwrap();

        // Switch the registered caller to the newly-added member and confirm
        // they can now write to the space.
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&new_member_did)).unwrap();
        let write_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local result = space:write_record{{
                    collection = "com.example.item",
                    record = {{ text = "from new member" }},
                }}
                return result.uri
            "#
        );
        let uri: String = lua.load(&write_chunk).eval_async().await.unwrap();
        assert!(
            uri.contains(&new_member_did),
            "expected record uri to be authored by the new member, got: {uri}"
        );
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn remove_member_revokes_write_access() {
        require_test_db!();
        let (state, space_uri, authority_did) = db_seeded_space().await;
        let new_member_did = format!("did:plc:removeme{}", uuid::Uuid::new_v4().simple());

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(
            &lua,
            state_arc.clone(),
            Some(&authority_did),
        )
        .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&authority_did)).unwrap();

        // authority adds the new member as a write-member
        let add_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                assert(space ~= nil, "space handle should not be nil")
                local member = space:add_member{{ did = "{new_member_did}", access = "write" }}
                assert(member.did == "{new_member_did}", "unexpected member did")
            "#
        );
        lua.load(&add_chunk).exec_async().await.unwrap();

        // confirm the new member can write while still a member
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&new_member_did)).unwrap();
        let write_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local result = space:write_record{{
                    collection = "com.example.item",
                    record = {{ text = "before removal" }},
                }}
                return result.uri
            "#
        );
        let uri: String = lua.load(&write_chunk).eval_async().await.unwrap();
        assert!(
            uri.contains(&new_member_did),
            "expected record uri to be authored by the new member, got: {uri}"
        );

        // authority removes the member
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&authority_did)).unwrap();
        let remove_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local ok = space:remove_member{{ did = "{new_member_did}" }}
                assert(ok == true, "remove_member should return true")
            "#
        );
        lua.load(&remove_chunk).exec_async().await.unwrap();

        // re-register as the removed member and confirm write_record now raises
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&new_member_did)).unwrap();
        let write_after_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local ok, err = pcall(function()
                    return space:write_record{{
                        collection = "com.example.item",
                        record = {{ text = "after removal" }},
                    }}
                end)
                assert(not ok, "write_record should fail after the member was removed")
                return tostring(err)
            "#
        );
        let err: String = lua.load(&write_after_chunk).eval_async().await.unwrap();
        assert!(
            err.to_lowercase().contains("not a member"),
            "expected a not-a-member error, got: {err}"
        );
    }

    /// A member who wrote a record, and is subsequently removed from the
    /// space, can no longer delete that record — even though they are still
    /// its author. `delete_record` requires *current* write-membership in
    /// addition to authorship; a removed member fails the membership check
    /// before the ownership check is ever reached.
    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn delete_record_rejects_removed_member() {
        require_test_db!();
        let (state, space_uri, authority_did) = db_seeded_space().await;
        let new_member_did = format!("did:plc:delremoveme{}", uuid::Uuid::new_v4().simple());

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(
            &lua,
            state_arc.clone(),
            Some(&authority_did),
        )
        .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&authority_did)).unwrap();

        // authority adds the new member as a write-member
        let add_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                assert(space ~= nil, "space handle should not be nil")
                local member = space:add_member{{ did = "{new_member_did}", access = "write" }}
                assert(member.did == "{new_member_did}", "unexpected member did")
            "#
        );
        lua.load(&add_chunk).exec_async().await.unwrap();

        // the new member writes a record they author
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&new_member_did)).unwrap();
        let write_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local result = space:write_record{{
                    collection = "com.example.item",
                    record = {{ text = "written before removal" }},
                }}
                return result.uri
            "#
        );
        let uri: String = lua.load(&write_chunk).eval_async().await.unwrap();
        assert!(
            uri.contains(&new_member_did),
            "expected record uri to be authored by the new member, got: {uri}"
        );
        let rkey = uri.rsplit('/').next().unwrap().to_string();

        // authority removes the member
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&authority_did)).unwrap();
        let remove_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local ok = space:remove_member{{ did = "{new_member_did}" }}
                assert(ok == true, "remove_member should return true")
            "#
        );
        lua.load(&remove_chunk).exec_async().await.unwrap();

        // re-register as the removed member and confirm delete_record on
        // their own previously-authored record now raises, even though they
        // are still its author.
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&new_member_did)).unwrap();
        let delete_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local ok, err = pcall(function()
                    return space:delete_record{{
                        collection = "com.example.item",
                        rkey = "{rkey}",
                    }}
                end)
                assert(not ok, "delete_record should fail after the member was removed")
                return tostring(err)
            "#
        );
        let err: String = lua.load(&delete_chunk).eval_async().await.unwrap();
        assert!(
            err.to_lowercase().contains("not a member"),
            "expected a not-a-member error, got: {err}"
        );
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn create_space_edge_cases() {
        require_test_db!();
        let state = db_test_state().await;
        enable_spaces_feature(&state).await;

        let unique = uuid::Uuid::new_v4().simple().to_string();
        let creator_did = format!("did:plc:luaedge{unique}");
        let type_nsid = "com.example.edge";
        let skey = format!("dup{unique}");

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(&lua, state_arc.clone(), Some(&creator_did))
            .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&creator_did)).unwrap();

        // (a) creating a space with an already-used type/skey raises Conflict
        let create_chunk = format!(
            r#"
                local space = atproto.spaces.create{{
                    type = "{type_nsid}",
                    skey = "{skey}",
                }}
                assert(space ~= nil, "first create should succeed")
            "#
        );
        lua.load(&create_chunk).exec_async().await.unwrap();

        let dup_chunk = format!(
            r#"
                local ok, err = pcall(function()
                    return atproto.spaces.create{{
                        type = "{type_nsid}",
                        skey = "{skey}",
                    }}
                end)
                assert(not ok, "creating a duplicate type/skey should fail")
                return tostring(err)
            "#
        );
        let err: String = lua.load(&dup_chunk).eval_async().await.unwrap();
        assert!(
            err.to_lowercase().contains("already exists"),
            "expected an 'already exists' error, got: {err}"
        );

        // (b) an invalid mint_policy raises before ever reaching the service layer
        let bad_policy_chunk = format!(
            r#"
                local ok, err = pcall(function()
                    return atproto.spaces.create{{
                        type = "{type_nsid}",
                        skey = "{skey}-other",
                        mint_policy = "not-a-real-policy",
                    }}
                end)
                assert(not ok, "creating with an invalid mint_policy should fail")
                return tostring(err)
            "#
        );
        let err: String = lua.load(&bad_policy_chunk).eval_async().await.unwrap();
        assert!(
            err.to_lowercase().contains("invalid mint_policy"),
            "expected an 'invalid mint_policy' error, got: {err}"
        );
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn add_member_raises_for_non_admin() {
        require_test_db!();
        let (state, space_uri, _authority_did) = db_seeded_space().await;
        let stranger_did = format!("did:plc:stranger{}", uuid::Uuid::new_v4().simple());

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(&lua, state_arc.clone(), Some(&stranger_did))
            .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&stranger_did)).unwrap();

        let chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local ok, err = pcall(function()
                    return space:add_member{{ did = "did:plc:whoever", access = "read" }}
                end)
                assert(not ok, "add_member should fail for a non-admin caller")
                return tostring(err)
            "#
        );
        let err: String = lua.load(&chunk).eval_async().await.unwrap();
        assert!(
            err.contains("forbidden"),
            "expected a forbidden error, got: {err}"
        );
    }

    /// A `read`-access member can perform read operations (`query`,
    /// `members`) but is rejected by `write_record`/`delete_record`, which
    /// both call `require_membership(..., write=true)`. `SpaceAccess::can_write()`
    /// is `false` for `Read`/`ReadSelf`, so this exercises the read-vs-write
    /// boundary rather than the plain non-member rejection already covered
    /// by `add_member_raises_for_non_admin` / `remove_member_revokes_write_access`.
    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn read_only_member_can_read_but_not_write() {
        require_test_db!();
        let (state, space_uri, authority_did) = db_seeded_space().await;
        let reader_did = format!("did:plc:reader{}", uuid::Uuid::new_v4().simple());

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(
            &lua,
            state_arc.clone(),
            Some(&authority_did),
        )
        .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&authority_did)).unwrap();

        // authority adds the new member with read-only access
        let add_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                assert(space ~= nil, "space handle should not be nil")
                local member = space:add_member{{ did = "{reader_did}", access = "read" }}
                assert(member.did == "{reader_did}", "unexpected member did")
                assert(member.access == "read", "unexpected member access")
            "#
        );
        lua.load(&add_chunk).exec_async().await.unwrap();

        // re-register as the read-only member
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&reader_did)).unwrap();

        // reads succeed for a read-only member
        let read_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                assert(space ~= nil, "space handle should not be nil")
                local q = space:query{{}}
                assert(q.records ~= nil, "read-only member should be able to query")
                local members = space:members()
                assert(#members == 2, "expected two members")
                assert(space:access("{reader_did}") == "read", "unexpected access level")
            "#
        );
        lua.load(&read_chunk).exec_async().await.unwrap();

        // write_record raises Forbidden for a read-only member
        let write_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local ok, err = pcall(function()
                    return space:write_record{{
                        collection = "com.example.item",
                        record = {{ text = "should not be allowed" }},
                    }}
                end)
                assert(not ok, "write_record should fail for a read-only member")
                return tostring(err)
            "#
        );
        let err: String = lua.load(&write_chunk).eval_async().await.unwrap();
        assert!(
            err.contains("forbidden"),
            "expected a forbidden error from write_record, got: {err}"
        );

        // delete_record raises Forbidden for a read-only member
        // (require_membership rejects before the record-ownership check is
        // ever reached, so this holds even for a non-existent rkey).
        let delete_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local ok, err = pcall(function()
                    return space:delete_record{{
                        collection = "com.example.item",
                        rkey = "doesnotexist",
                    }}
                end)
                assert(not ok, "delete_record should fail for a read-only member")
                return tostring(err)
            "#
        );
        let err: String = lua.load(&delete_chunk).eval_async().await.unwrap();
        assert!(
            err.contains("forbidden"),
            "expected a forbidden error from delete_record, got: {err}"
        );
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn update_persists_changes_and_clears_with_false() {
        require_test_db!();
        let (state, space_uri, authority_did) = db_seeded_space().await;

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(
            &lua,
            state_arc.clone(),
            Some(&authority_did),
        )
        .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&authority_did)).unwrap();

        // Set display_name, then confirm it persisted via atproto.spaces.get.
        let set_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                assert(space ~= nil, "space handle should not be nil")
                local ok = space:update{{ display_name = "New" }}
                assert(ok == true, "update should return true")
            "#
        );
        lua.load(&set_chunk).exec_async().await.unwrap();

        // The LuaSpace handle doesn't expose display_name directly, so assert
        // persistence via the underlying service resolver (same path
        // `atproto.spaces.get` uses).
        let persisted = crate::spaces::service::resolve_space(&state_arc, &space_uri)
            .await
            .unwrap();
        assert_eq!(persisted.display_name.as_deref(), Some("New"));

        // Clear display_name via `false`.
        let clear_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local ok = space:update{{ display_name = false }}
                assert(ok == true, "update should return true")
            "#
        );
        lua.load(&clear_chunk).exec_async().await.unwrap();
        let cleared = crate::spaces::service::resolve_space(&state_arc, &space_uri)
            .await
            .unwrap();
        assert_eq!(cleared.display_name, None);

        // Delete the space, then confirm atproto.spaces.get returns nil.
        let delete_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local ok = space:delete()
                assert(ok == true, "delete should return true")
                local gone = atproto.spaces.get("{space_uri}")
                return gone == nil
            "#
        );
        let is_gone: bool = lua.load(&delete_chunk).eval_async().await.unwrap();
        assert!(is_gone, "space should be gone after delete");
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn update_and_delete_raise_for_non_admin() {
        require_test_db!();
        let (state, space_uri, _authority_did) = db_seeded_space().await;
        let stranger_did = format!("did:plc:stranger{}", uuid::Uuid::new_v4().simple());

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(&lua, state_arc.clone(), Some(&stranger_did))
            .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&stranger_did)).unwrap();

        let update_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local ok, err = pcall(function()
                    return space:update{{ display_name = "hacked" }}
                end)
                assert(not ok, "update should fail for a non-admin caller")
                return tostring(err)
            "#
        );
        let err: String = lua.load(&update_chunk).eval_async().await.unwrap();
        assert!(
            err.contains("forbidden"),
            "expected a forbidden error, got: {err}"
        );

        let delete_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local ok, err = pcall(function()
                    return space:delete()
                end)
                assert(not ok, "delete should fail for a non-admin caller")
                return tostring(err)
            "#
        );
        let err: String = lua.load(&delete_chunk).eval_async().await.unwrap();
        assert!(
            err.contains("forbidden"),
            "expected a forbidden error, got: {err}"
        );
    }

    /// Extends `update_persists_changes_and_clears_with_false` (which only
    /// covers `display_name` set/clear) to the structured fields: `app_access`
    /// (tagged enum), `config` (struct with real fields + a flattened extra
    /// key), `mint_policy`, and clearing `managing_app_did` with `false`.
    /// Also asserts an invalid `mint_policy` string raises on `update`, mirroring
    /// the create-side assertion in `create_space_edge_cases`.
    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn update_persists_structured_fields() {
        require_test_db!();
        let (state, space_uri, authority_did) = db_seeded_space().await;

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(
            &lua,
            state_arc.clone(),
            Some(&authority_did),
        )
        .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&authority_did)).unwrap();

        // First SET managing_app_did to a real value and confirm it persisted, so
        // the subsequent `false`-clear is a genuine round-trip (the seeded space
        // has managing_app_did = None, so asserting None alone would pass even if
        // the clear path were broken).
        let set_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                assert(space ~= nil, "space handle should not be nil")
                assert(space:update{{ managing_app_did = "did:plc:managingapp" }} == true)
            "#
        );
        lua.load(&set_chunk).exec_async().await.unwrap();
        let after_set = crate::spaces::service::resolve_space(&state_arc, &space_uri)
            .await
            .unwrap();
        assert_eq!(
            after_set.managing_app_did,
            Some("did:plc:managingapp".to_string()),
            "expected managing_app_did to be set before the clear"
        );

        let update_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                assert(space ~= nil, "space handle should not be nil")
                local ok = space:update{{
                    app_access = {{ type = "allowList", allowed = {{ "did:plc:x" }} }},
                    config = {{ membership_public = true, records_public = true, custom_flag = "yep" }},
                    mint_policy = "public",
                    managing_app_did = false,
                }}
                assert(ok == true, "update should return true")
            "#
        );
        lua.load(&update_chunk).exec_async().await.unwrap();

        let persisted = crate::spaces::service::resolve_space(&state_arc, &space_uri)
            .await
            .unwrap();

        match &persisted.app_access {
            crate::spaces::types::AppAccess::AllowList { allowed } => {
                assert_eq!(allowed, &vec!["did:plc:x".to_string()]);
            }
            other => panic!("expected AllowList app_access, got {other:?}"),
        }
        assert!(
            persisted.config.membership_public,
            "expected membership_public to persist as true"
        );
        assert!(
            persisted.config.records_public,
            "expected records_public to persist as true"
        );
        assert_eq!(
            persisted.config.extra.get("custom_flag"),
            Some(&serde_json::Value::String("yep".to_string())),
            "expected the flattened extra field to persist"
        );
        assert_eq!(
            persisted.mint_policy,
            crate::spaces::types::MintPolicy::Public
        );
        assert_eq!(
            persisted.managing_app_did, None,
            "expected managing_app_did to be cleared by `false`"
        );

        // An invalid mint_policy raises before ever reaching the service layer.
        let bad_policy_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local ok, err = pcall(function()
                    return space:update{{ mint_policy = "bogus" }}
                end)
                assert(not ok, "update with an invalid mint_policy should fail")
                return tostring(err)
            "#
        );
        let err: String = lua.load(&bad_policy_chunk).eval_async().await.unwrap();
        assert!(
            err.to_lowercase().contains("invalid mint_policy"),
            "expected an 'invalid mint_policy' error, got: {err}"
        );
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn members_and_query_work_without_caller() {
        require_test_db!();
        let (state, space_uri, authority_did) = db_seeded_space().await;

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(&lua, state_arc.clone(), None).unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), None).unwrap();

        let chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                assert(space ~= nil, "space handle should not be nil")
                local members = space:members()
                assert(#members == 1, "expected exactly one member")
                assert(members[1].did == "{authority_did}", "unexpected member did")
                assert(space:is_member("{authority_did}") == true, "authority should be a member")
                assert(space:access("{authority_did}") == "write", "unexpected access level")
                local q = space:query{{}}
                assert(q.records ~= nil, "query should return a records table")
                return members[1].access
            "#
        );
        let access: String = lua.load(&chunk).eval_async().await.unwrap();
        assert_eq!(access, "write");
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn read_methods_gate_on_feature_flag() {
        require_test_db!();
        let state = db_test_state().await;
        // Intentionally do NOT call enable_spaces_feature(&state). The flag
        // row is a single global row shared by every test against this DB,
        // so other (non-serialized-with-us) tests could have left it
        // enabled from a previous run against a persistent test database;
        // explicitly force it back to disabled so this test is
        // deterministic regardless of history or execution order. This
        // test is tagged `#[serial(spaces_feature_flag)]` along with every
        // test that calls `enable_spaces_feature`, so none of them can race
        // with this flip.
        disable_spaces_feature(&state).await;

        let unique = uuid::Uuid::new_v4().simple().to_string();
        let space_id = uuid::Uuid::new_v4().to_string();
        let member_did = format!("did:plc:luawriter{unique}");
        let space_did = format!("did:plc:luaowner{unique}");
        let type_nsid = "com.example.forum";
        let skey = format!("main{unique}");

        let space = crate::spaces::types::Space {
            id: space_id.clone(),
            did: space_did.clone(),
            authority_did: member_did.clone(),
            creator_did: member_did.clone(),
            type_nsid: type_nsid.to_string(),
            skey: skey.clone(),
            display_name: None,
            description: None,
            mint_policy: crate::spaces::types::MintPolicy::MemberList,
            app_access: crate::spaces::types::AppAccess::default(),
            managing_app_did: None,
            config: crate::spaces::types::SpaceConfig::default(),
            revision: None,
            created_at: crate::db::now_rfc3339(),
            updated_at: crate::db::now_rfc3339(),
        };
        crate::spaces::db::create_space(&state.db, state.db_backend, &space)
            .await
            .expect("failed to seed test space");

        let space_uri = format!("at://{space_did}/space/{type_nsid}/{skey}");

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(&lua, state_arc.clone(), None).unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), None).unwrap();

        // Use __test_handle rather than atproto.spaces.get(), since get()
        // itself gates on the feature flag and wouldn't return a handle.
        let chunk = format!(
            r#"
                local space = atproto.spaces.__test_handle("{space_uri}")
                local ok, err = pcall(function()
                    return space:query{{}}
                end)
                assert(not ok, "query should fail when spaces feature flag is disabled")
                return tostring(err)
            "#
        );
        let err: String = lua.load(&chunk).eval_async().await.unwrap();
        assert!(
            err.contains("spaces feature is not enabled"),
            "expected feature-flag error, got: {err}"
        );
    }

    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn create_invite_then_accept_invite_lets_joiner_write() {
        require_test_db!();
        let (state, space_uri, authority_did) = db_seeded_space().await;
        let joiner_did = format!("did:plc:joiner{}", uuid::Uuid::new_v4().simple());

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(
            &lua,
            state_arc.clone(),
            Some(&authority_did),
        )
        .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&authority_did)).unwrap();

        let invite_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                assert(space ~= nil, "space handle should not be nil")
                local invite = space:create_invite{{ access = "write" }}
                assert(invite.token ~= nil, "create_invite should return a token")
                assert(invite.access == "write", "unexpected invite access")
                return invite.token
            "#
        );
        let token: String = lua.load(&invite_chunk).eval_async().await.unwrap();

        // Re-register with the joiner as the caller and accept the invite.
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&joiner_did)).unwrap();
        let accept_chunk = format!(
            r#"
                local space = atproto.spaces.accept_invite{{ token = "{token}" }}
                assert(space ~= nil, "accept_invite should return a handle")
                assert(space.uri == "{space_uri}", "unexpected joined space uri")
                local result = space:write_record{{
                    collection = "com.example.item",
                    record = {{ text = "from joiner" }},
                }}
                return result.uri
            "#
        );
        let uri: String = lua.load(&accept_chunk).eval_async().await.unwrap();
        assert!(
            uri.contains(&joiner_did),
            "expected record uri to be authored by the joiner, got: {uri}"
        );
    }

    /// `space:query{}` pagination: `list_space_records` returns a `cursor`
    /// exactly when the page is full (`records.len() == limit`), so writing
    /// a count that isn't an exact multiple of the page size (5 records at
    /// 2/page: pages of 2, 2, 1) exercises both the "more pages" and the
    /// "final short page -> nil cursor" paths. Walks the cursor until nil and
    /// asserts every written record is seen exactly once, with no overlap
    /// between pages.
    #[tokio::test]
    #[serial(spaces_feature_flag)]
    async fn query_paginates_with_cursor() {
        require_test_db!();
        let (state, space_uri, member_did) = db_seeded_space().await;
        let collection = "com.example.paginated";

        let lua = mlua::Lua::new();
        let state_arc = std::sync::Arc::new(state);
        crate::lua::atproto_api::register_atproto_api(&lua, state_arc.clone(), Some(&member_did))
            .unwrap();
        super::register_spaces_write_api(&lua, state_arc.clone(), Some(&member_did)).unwrap();

        let write_chunk = format!(
            r#"
                local space = atproto.spaces.get("{space_uri}")
                local uris = {{}}
                for i = 1, 5 do
                    local result = space:write_record{{
                        collection = "{collection}",
                        record = {{ text = "item " .. i }},
                    }}
                    uris[i] = result.uri
                end
                return uris
            "#
        );
        let written_table: mlua::Table = lua.load(&write_chunk).eval_async().await.unwrap();
        let written: Vec<String> = written_table
            .sequence_values::<String>()
            .collect::<mlua::Result<_>>()
            .unwrap();
        assert_eq!(written.len(), 5, "expected 5 records to have been written");

        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            pages += 1;
            assert!(pages <= 10, "pagination did not terminate within 10 pages");

            let cursor_literal = match &cursor {
                Some(c) => format!("\"{c}\""),
                None => "nil".to_string(),
            };
            let page_chunk = format!(
                r#"
                    local space = atproto.spaces.get("{space_uri}")
                    local q = space:query{{
                        collection = "{collection}",
                        limit = 2,
                        cursor = {cursor_literal},
                    }}
                    local uris = {{}}
                    for i, r in ipairs(q.records) do
                        uris[i] = r.uri
                    end
                    return {{ uris = uris, cursor = q.cursor }}
                "#
            );
            let page: mlua::Table = lua.load(&page_chunk).eval_async().await.unwrap();
            let page_uris_table: mlua::Table = page.get("uris").unwrap();
            let page_uris: Vec<String> = page_uris_table
                .sequence_values::<String>()
                .collect::<mlua::Result<_>>()
                .unwrap();
            let next_cursor: Option<String> = page.get("cursor").unwrap();

            assert!(
                page_uris.len() <= 2,
                "expected at most 2 records per page, got {}",
                page_uris.len()
            );
            if cursor.is_none() {
                assert_eq!(page_uris.len(), 2, "first page should be full (2 records)");
                assert!(next_cursor.is_some(), "first page should return a cursor");
            }

            for uri in &page_uris {
                assert!(
                    !seen.contains(uri),
                    "record {uri} was returned on more than one page"
                );
                seen.push(uri.clone());
            }

            match next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        let mut seen_sorted = seen.clone();
        seen_sorted.sort();
        let mut expected_sorted = written.clone();
        expected_sorted.sort();
        assert_eq!(
            seen_sorted, expected_sorted,
            "expected every written record to be seen exactly once across pages"
        );
    }
}
