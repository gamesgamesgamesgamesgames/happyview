//! Raw SQL for plugins. Same boundary as Lua's `db.raw`: the table guard in
//! `raw_sql_guard` protects HappyView's internal tables; the capability decides
//! whether writes are allowed at all.

use serde_json::{Map, Value};
use sqlx::{Column, Row};

use crate::raw_sql_guard::check_raw_sql_tables;

fn bind_params<'q>(
    mut query: sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments>,
    params: &'q [Value],
) -> Result<sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments>, String> {
    for value in params {
        query = match value {
            Value::String(s) => query.bind(s.as_str()),
            Value::Number(n) if n.is_i64() => query.bind(n.as_i64().unwrap()),
            Value::Number(n) => query.bind(n.as_f64().unwrap_or(0.0)),
            // A boolean binds as one: Postgres will not compare its own
            // `boolean` against an integer, and SQLite stores the bind as the
            // 0/1 an integer column already holds.
            Value::Bool(b) => query.bind(*b),
            Value::Null => query.bind(Option::<String>::None),
            other => return Err(format!("unsupported parameter type: {other}")),
        };
    }
    Ok(query)
}

pub(super) fn row_to_json(row: &sqlx::any::AnyRow) -> Map<String, Value> {
    let mut out = Map::new();
    for col in row.columns() {
        let name = col.name();
        let v = if let Ok(s) = row.try_get::<String, _>(name) {
            Value::String(s)
        } else if let Ok(n) = row.try_get::<i64, _>(name) {
            Value::from(n)
        } else if let Ok(n) = row.try_get::<i32, _>(name) {
            Value::from(n)
        } else if let Ok(n) = row.try_get::<f64, _>(name) {
            Value::from(n)
        } else if let Ok(b) = row.try_get::<bool, _>(name) {
            Value::Bool(b)
        } else {
            Value::Null
        };
        out.insert(name.to_string(), v);
    }
    out
}

/// Why a raw query did not run: the guard or a parameter refused it, or the
/// database failed it. Callers that route a query to a database that may be
/// unavailable need the second apart from the first.
#[derive(Debug)]
pub enum QueryError {
    Refused(String),
    Database(sqlx::Error),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryError::Refused(m) => f.write_str(m),
            QueryError::Database(e) => write!(f, "query failed: {e}"),
        }
    }
}

/// [`run_query`] with the failure kept typed.
pub async fn run_query_on(
    db: &sqlx::AnyPool,
    sql: &str,
    params: &[Value],
) -> Result<Vec<Map<String, Value>>, QueryError> {
    check_raw_sql_tables(sql).map_err(QueryError::Refused)?;
    let rows = bind_params(crate::db::query(sql), params)
        .map_err(QueryError::Refused)?
        .fetch_all(db)
        .await
        .map_err(QueryError::Database)?;
    Ok(rows.iter().map(row_to_json).collect())
}

/// [`run_query_on`] for a read routed to a database another owner also
/// uses. On Postgres it runs in a read-only transaction that is rolled back,
/// so it can neither write (`nextval`, say) nor leave session state
/// (`set_config`) on a pooled connection that owner's own work reuses.
pub async fn run_read_only_query_on(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    sql: &str,
    params: &[Value],
) -> Result<Vec<Map<String, Value>>, QueryError> {
    if backend == crate::db::DatabaseBackend::Sqlite {
        return run_query_on(db, sql, params).await;
    }
    check_raw_sql_tables(sql).map_err(QueryError::Refused)?;
    let query = bind_params(crate::db::query(sql), params).map_err(QueryError::Refused)?;
    let mut tx = db.begin().await.map_err(QueryError::Database)?;
    crate::db::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(QueryError::Database)?;
    let rows = query
        .fetch_all(&mut *tx)
        .await
        .map_err(QueryError::Database)?;
    tx.rollback().await.map_err(QueryError::Database)?;
    Ok(rows.iter().map(row_to_json).collect())
}

/// SQL is passed through untranslated; placeholders are backend-native
/// (`?` on SQLite, `$1`… on Postgres), as for Lua's `db.raw`.
pub async fn run_query(
    db: &sqlx::AnyPool,
    sql: &str,
    params: &[Value],
) -> Result<Vec<Map<String, Value>>, String> {
    run_query_on(db, sql, params)
        .await
        .map_err(|e| e.to_string())
}

/// SQL is passed through untranslated; placeholders are backend-native
/// (`?` on SQLite, `$1`… on Postgres), as for Lua's `db.raw`.
pub async fn run_execute(db: &sqlx::AnyPool, sql: &str, params: &[Value]) -> Result<u64, String> {
    check_raw_sql_tables(sql)?;
    let result = bind_params(crate::db::query(sql), params)?
        .execute(db)
        .await
        .map_err(|e| format!("execute failed: {e}"))?;
    Ok(result.rows_affected())
}

// -- Least-privileged plugin SQL (Postgres) ---------------------------------
//
// On Postgres the tokenizer table guard works on identifiers only, so it is not
// the boundary for plugin raw SQL. Two things make the database enforce it:
//
//  1. `raw_sql_guard::check_postgres_plugin_sql` parses with Postgres's own
//     parser and accepts only allowlisted statement kinds, relations and
//     functions.
//  2. the statement runs under a role (`happyview_plugin_ro`/`_rw`) GRANTed only
//     the allowlisted relations, so anything off the allowlist is denied by the
//     server, as that role, however the statement is written.
//
// (1) is applied always; (2) when the roles could be set up at boot. When they
// could not, (1) is the whole boundary and the statement runs on the privileged
// connection, still in a read-only transaction for reads. SQLite keeps the
// tokenizer guard and the plain run paths, which are sufficient there.

/// The least-privileged role a read-only plugin statement runs under.
pub const PLUGIN_ROLE_RO: &str = "happyview_plugin_ro";
/// The least-privileged role a writing plugin statement runs under.
pub const PLUGIN_ROLE_RW: &str = "happyview_plugin_rw";
/// The optional LOGIN role the login pool connects as; a member of both bundles.
pub const PLUGIN_ROLE_LOGIN: &str = "happyview_plugin";

/// Run a plugin query on `db` under the enforcement `enforcement` describes.
/// `db` is the login pool in [`PluginSqlEnforcement::LoginPool`] and the main
/// pool otherwise; `mode` picks the role and, for a read, a rolled-back
/// read-only transaction. SQLite keeps [`run_query_on`].
pub async fn run_plugin_query(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    enforcement: crate::raw_sql_guard::PluginSqlEnforcement,
    mode: crate::raw_sql_guard::PluginSqlMode,
    sql: &str,
    params: &[Value],
) -> Result<Vec<Map<String, Value>>, String> {
    use crate::raw_sql_guard::PluginSqlMode;
    if backend == crate::db::DatabaseBackend::Sqlite {
        return run_query(db, sql, params).await;
    }
    crate::raw_sql_guard::check_postgres_plugin_sql(sql, mode, enforcement)?;
    let role = match mode {
        PluginSqlMode::Read => PLUGIN_ROLE_RO,
        PluginSqlMode::Write => PLUGIN_ROLE_RW,
    };
    let read_only = mode == PluginSqlMode::Read;
    let query = bind_params(crate::db::query(sql), params)?;
    let mut tx = db.begin().await.map_err(|e| format!("query failed: {e}"))?;
    enter_plugin_role(&mut tx, role, enforcement, read_only).await?;
    let rows = query
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("query failed: {e}"))?;
    if read_only {
        // A `database:read` query ran in a read-only transaction; roll it back.
        tx.rollback()
            .await
            .map_err(|e| format!("query failed: {e}"))?;
    } else {
        // Under `database:write`, a statement run through the query import may
        // have written (e.g. `INSERT … RETURNING`), so commit as the pre-role
        // autocommit path did.
        tx.commit()
            .await
            .map_err(|e| format!("query failed: {e}"))?;
    }
    Ok(rows.iter().map(row_to_json).collect())
}

/// Run a plugin write under `happyview_plugin_rw` (committing on success) with
/// the enforcement `enforcement` describes. SQLite keeps [`run_execute`].
pub async fn run_plugin_execute(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    enforcement: crate::raw_sql_guard::PluginSqlEnforcement,
    sql: &str,
    params: &[Value],
) -> Result<u64, String> {
    if backend == crate::db::DatabaseBackend::Sqlite {
        return run_execute(db, sql, params).await;
    }
    crate::raw_sql_guard::check_postgres_plugin_sql(
        sql,
        crate::raw_sql_guard::PluginSqlMode::Write,
        enforcement,
    )?;
    let query = bind_params(crate::db::query(sql), params)?;
    let mut tx = db
        .begin()
        .await
        .map_err(|e| format!("execute failed: {e}"))?;
    enter_plugin_role(&mut tx, PLUGIN_ROLE_RW, enforcement, false).await?;
    let result = query
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("execute failed: {e}"))?;
    let affected = result.rows_affected();
    tx.commit()
        .await
        .map_err(|e| format!("execute failed: {e}"))?;
    Ok(affected)
}

/// Inside the plugin statement's own transaction, switch to the least-privileged
/// role and pin `search_path`, both with `SET LOCAL` so neither leaks onto the
/// pooled connection the next caller reuses. The role is switched in both the
/// login pool (restricting the login role further) and role mode; in
/// `ParserOnly` there is no role to switch to. For a read, also mark the
/// transaction read-only before any query runs.
async fn enter_plugin_role(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    role: &str,
    enforcement: crate::raw_sql_guard::PluginSqlEnforcement,
    read_only: bool,
) -> Result<(), String> {
    use crate::raw_sql_guard::PluginSqlEnforcement;
    if read_only {
        crate::db::query("SET TRANSACTION READ ONLY")
            .execute(&mut **tx)
            .await
            .map_err(|e| format!("query failed: {e}"))?;
    }
    // In parser-only mode the privileged connection must NOT put the plugin
    // schema on its path: an object a plugin left there in an earlier login-pool
    // session would otherwise be resolved first. (Boot also refuses plugin SQL
    // entirely when that schema holds functions or triggers.) The role and login
    // modes put the plugin schema first, where a plugin's own objects live.
    let search_path = if enforcement == PluginSqlEnforcement::ParserOnly {
        "public".to_string()
    } else {
        // `role` is one of our own constants, never caller input.
        crate::db::query(&format!("SET LOCAL ROLE {role}"))
            .execute(&mut **tx)
            .await
            .map_err(|e| format!("query failed: {e}"))?;
        format!("{PLUGIN_SCHEMA}, public")
    };
    crate::db::query(&format!("SET LOCAL search_path = {search_path}"))
        .execute(&mut **tx)
        .await
        .map_err(|e| format!("query failed: {e}"))?;
    Ok(())
}

/// Ensure the two least-privileged plugin roles and the plugin schema exist and
/// hold grants on exactly the currently-allowlisted relations, and that the
/// connecting role may `SET ROLE` to them. Postgres only; returns whether plugin
/// SQL can run under them. Idempotent and safe to run at every boot. A relation
/// dropped from the allowlist across versions keeps a stale grant, but the parser
/// refuses its name, so it stays unreachable.
///
/// Fails **soft**: if the connecting role lacks the privilege to create or grant
/// (no `CREATEROLE`, not the tables' owner), the setup statements are swallowed
/// and the function returns `false` once the `SET ROLE` probe fails. Plugin SQL
/// then falls back to the parser allowlist on the privileged connection, and the
/// caller logs how to enable the roles.
pub async fn ensure_plugin_roles(
    pool: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> bool {
    if backend != crate::db::DatabaseBackend::Postgres {
        return false;
    }

    // Best-effort setup. Each statement is independent and tolerant: a
    // pre-provisioned role (operator-created) may already exist and already be
    // granted, so a failure here is not fatal — the SET ROLE probe below is the
    // real test of whether plugin SQL can use the roles.
    for stmt in plugin_role_setup_statements() {
        if let Err(e) = crate::db::query(&stmt).execute(pool).await {
            tracing::debug!(error = %e, statement = %stmt, "plugin role setup statement did not apply");
        }
    }

    // Probe: can we actually assume each role? This is what a plugin statement
    // will do. It is the single source of truth for availability, so it covers
    // both the self-created and the operator-provisioned paths.
    for role in [PLUGIN_ROLE_RO, PLUGIN_ROLE_RW] {
        let probe = async {
            let mut tx = pool.begin().await.ok()?;
            crate::db::query(&format!("SET LOCAL ROLE {role}"))
                .execute(&mut *tx)
                .await
                .ok()?;
            tx.rollback().await.ok()?;
            Some(())
        };
        if probe.await.is_none() {
            return false;
        }
    }
    true
}

/// Decide how plugin raw SQL is isolated on Postgres, once at boot. Sets up the
/// roles and schema, then: if `login_url` (`PLUGIN_SQL_DATABASE_URL`) is set,
/// opens and **verifies** the login pool and **fails closed** (refuses plugin
/// SQL) rather than downgrading if it cannot; otherwise refuses when the plugin
/// schema holds code from an earlier login-pool session, else runs in role or
/// parser-only mode. SQLite is always parser-only (ignored there).
pub async fn decide_plugin_sql_access(
    db_pool: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    login_url: Option<&str>,
) -> crate::PluginSqlAccess {
    use crate::PluginSqlAccess;
    if backend != crate::db::DatabaseBackend::Postgres {
        return PluginSqlAccess::ParserOnly;
    }
    let restricted = ensure_plugin_roles(db_pool, backend).await;

    if let Some(url) = login_url.map(str::trim).filter(|u| !u.is_empty()) {
        let main_role = main_role_name(db_pool).await;
        return match open_plugin_login_pool(url, &main_role).await {
            Some(pool) => {
                tracing::info!(
                    "plugin raw SQL runs on the dedicated login pool as an unprivileged role"
                );
                PluginSqlAccess::LoginPool(pool)
            }
            None => {
                tracing::error!(
                    "PLUGIN_SQL_DATABASE_URL is set but the plugin SQL login pool could not be \
                     opened or verified; refusing plugin raw SQL on Postgres. HappyView otherwise \
                     boots normally."
                );
                PluginSqlAccess::Refused
            }
        };
    }

    // Outside the login pool, objects a plugin left in its schema would run with
    // the privileged session user, so refuse plugin SQL until they are cleared
    // (or the login pool is restored).
    if plugin_schema_has_code(db_pool, backend).await {
        tracing::error!(
            "the '{PLUGIN_SCHEMA}' schema holds functions or triggers created in an earlier \
             login-pool session; refusing plugin raw SQL (they would otherwise run with \
             HappyView's privileges). Restore PLUGIN_SQL_DATABASE_URL, or drop those objects \
             from the '{PLUGIN_SCHEMA}' schema, to re-enable plugin SQL."
        );
        return PluginSqlAccess::Refused;
    }

    if restricted {
        tracing::info!(
            "plugin raw SQL runs under the least-privileged roles on the main connection"
        );
        PluginSqlAccess::Role
    } else {
        tracing::warn!(
            "plugin raw SQL runs under the parser allowlist on the main connection: the roles \
             '{PLUGIN_ROLE_RO}' / '{PLUGIN_ROLE_RW}' could not be set up. Grant the HappyView \
             database role CREATEROLE (or pre-create those roles and grant them to it), or set \
             PLUGIN_SQL_DATABASE_URL to the dedicated login role, to enable the stronger modes."
        );
        PluginSqlAccess::ParserOnly
    }
}

/// The main pool's login role (`current_user`), for the login-pool role check.
async fn main_role_name(pool: &sqlx::AnyPool) -> String {
    match crate::db::query("SELECT current_user::text AS u")
        .fetch_optional(pool)
        .await
    {
        Ok(Some(row)) => row.try_get::<String, _>("u").unwrap_or_default(),
        _ => String::new(),
    }
}

/// Open the dedicated login pool for plugin SQL from `url` (a connection string
/// for the `happyview_plugin` LOGIN role). Each released connection is reset
/// ([`LOGIN_POOL_RESET`]), so no temporary object, advisory lock, cursor or
/// session setting a plugin statement left behind outlives it on the pooled
/// connection.
///
/// **The role is verified, not trusted.** A URL that merely connects is not
/// enough: if it named a privileged role (a copy of `DATABASE_URL`, say), the
/// login pool's permissive rules would run on a privileged session user — the
/// condition the pool exists to rule out. So after connecting, the session user
/// is checked against `main_role`; the pool is refused unless it is unprivileged,
/// not the main role or a member of it, has no access to the protected tables and
/// no `CREATE` on `public`. Returns `None` (logged) when the pool cannot connect
/// or fails the check. Postgres only.
pub async fn open_plugin_login_pool(url: &str, main_role: &str) -> Option<sqlx::AnyPool> {
    crate::db::install_drivers();
    let pool = plugin_login_pool_options()
        .connect(url)
        .await
        .map_err(|e| tracing::warn!(error = %e, "could not open the plugin SQL login pool"))
        .ok()?;
    if !login_role_is_unprivileged(&pool, main_role).await {
        return None;
    }
    Some(pool)
}

/// What a released login-pool connection is reset with: `DISCARD ALL` without
/// its `DEALLOCATE ALL`. The driver keeps its own cache of the statements it
/// has prepared on a connection, so dropping them on the server would leave the
/// next caller to run one that no longer exists. Every plugin statement
/// prepares the same role and `search_path` statements, so that would fail on
/// almost every reused connection. Prepared statements carry no session state
/// a plugin could leave, and a plugin cannot prepare its own.
const LOGIN_POOL_RESET: &str = "CLOSE ALL; SET SESSION AUTHORIZATION DEFAULT; RESET ALL; \
     UNLISTEN *; SELECT pg_advisory_unlock_all(); DISCARD PLANS; DISCARD TEMP; \
     DISCARD SEQUENCES;";

/// The login pool's options, including the [`LOGIN_POOL_RESET`] on release.
fn plugin_login_pool_options() -> sqlx::any::AnyPoolOptions {
    sqlx::any::AnyPoolOptions::new()
        .max_connections(8)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .idle_timeout(std::time::Duration::from_secs(300))
        .after_release(|conn, _meta| {
            Box::pin(async move {
                // Discard anything the previous plugin statement left on the
                // session, then keep the connection in the pool. One batch over
                // the simple protocol, so the reset itself is never prepared.
                sqlx::raw_sql(sqlx::AssertSqlSafe(LOGIN_POOL_RESET))
                    .execute(&mut *conn)
                    .await?;
                Ok(true)
            })
        })
}

/// Verify the pool's session user is a safe least-privileged role. A single
/// query answers every condition; any failure (or a query error) refuses the
/// mode.
async fn login_role_is_unprivileged(pool: &sqlx::AnyPool, main_role: &str) -> bool {
    // One row, one boolean column `ok`. `has_table_privilege`/`has_schema_privilege`
    // over the protected tables and `public`; role attributes from `pg_roles`;
    // identity and membership against the main role. The login role may be a
    // member (directly or through another role) of the two plugin roles only, so
    // a grant of anything else, such as a predefined `pg_*` role, refuses it.
    let sql = "SELECT (\
        NOT r.rolsuper AND NOT r.rolcreaterole AND NOT r.rolcreatedb \
        AND NOT r.rolreplication AND NOT r.rolbypassrls \
        AND session_user <> $1 \
        AND NOT pg_has_role(session_user, $1, 'MEMBER') \
        AND NOT EXISTS (SELECT 1 FROM pg_roles g \
            WHERE g.rolname NOT IN (session_user, $2, $3) \
            AND pg_has_role(session_user, g.oid, 'MEMBER')) \
        AND NOT has_table_privilege(session_user, 'happyview_users', 'SELECT') \
        AND NOT has_table_privilege(session_user, 'happyview_api_keys', 'SELECT') \
        AND NOT has_table_privilege(session_user, 'happyview_dpop_keys', 'SELECT') \
        AND NOT has_table_privilege(session_user, 'happyview_oauth_sessions', 'SELECT') \
        AND NOT has_schema_privilege(session_user, 'public', 'CREATE') \
    ) AS ok FROM pg_roles r WHERE r.rolname = session_user";
    let row = match crate::db::query(sql)
        .bind(main_role)
        .bind(PLUGIN_ROLE_RO)
        .bind(PLUGIN_ROLE_RW)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            tracing::error!("plugin SQL login pool: could not read the login role's attributes");
            return false;
        }
        Err(e) => {
            tracing::error!(error = %e, "plugin SQL login pool: role verification query failed");
            return false;
        }
    };
    let ok = row.try_get::<bool, _>("ok").unwrap_or(false);
    if !ok {
        tracing::error!(
            "PLUGIN_SQL_DATABASE_URL names a role that is not a safe least-privileged plugin \
             role (it is privileged, is the main role or a member of it, is a member of a \
             role other than the two plugin roles, can read protected tables, or can create \
             in public). Refusing the login pool. Point it at the \
             unprivileged 'happyview_plugin' role."
        );
    }
    ok
}

/// The schema a plugin's own tables, and (in the login pool) its own functions
/// and triggers, live in. It is first on the plugin roles' `search_path` and is
/// never on HappyView's own, so nothing a plugin defines there can be resolved by
/// HappyView's own queries.
pub const PLUGIN_SCHEMA: &str = "happyview_plugin";

/// Whether the plugin schema holds any function, procedure or (non-internal)
/// trigger. Such objects can only have been created in login-pool mode; if the
/// instance is now outside that mode they would run with the privileged session
/// user, so the caller refuses plugin SQL. Postgres only; a query error reads as
/// "present" (fail closed).
pub async fn plugin_schema_has_code(
    pool: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> bool {
    if backend != crate::db::DatabaseBackend::Postgres {
        return false;
    }
    let sql = "SELECT EXISTS (\
        SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
          WHERE n.nspname = 'happyview_plugin' \
        UNION ALL \
        SELECT 1 FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
          JOIN pg_namespace n ON n.oid = c.relnamespace \
          WHERE n.nspname = 'happyview_plugin' AND NOT t.tgisinternal \
    ) AS present";
    match crate::db::query(sql).fetch_optional(pool).await {
        Ok(Some(row)) => row.try_get::<bool, _>("present").unwrap_or(true),
        Ok(None) => false,
        Err(e) => {
            tracing::error!(error = %e, "could not check the plugin schema for code; failing closed");
            true
        }
    }
}

/// The idempotent statements that create and (re)grant the plugin roles and the
/// plugin schema. Kept as data so a test can read the grant targets back.
fn plugin_role_setup_statements() -> Vec<String> {
    let relations = crate::raw_sql_guard::plugin_nameable_relations();

    let mut stmts = vec![
        format!(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{PLUGIN_ROLE_RO}') THEN CREATE ROLE {PLUGIN_ROLE_RO} NOLOGIN; END IF; END $$"
        ),
        format!(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{PLUGIN_ROLE_RW}') THEN CREATE ROLE {PLUGIN_ROLE_RW} NOLOGIN; END IF; END $$"
        ),
        format!("GRANT {PLUGIN_ROLE_RO}, {PLUGIN_ROLE_RW} TO CURRENT_USER"),
        // When a separate LOGIN role for the login pool has been provisioned,
        // give it the two privilege bundles so it inherits exactly their access
        // and nothing more. It stays absent on instances that don't use the pool.
        format!(
            "DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{PLUGIN_ROLE_LOGIN}') \
             THEN GRANT {PLUGIN_ROLE_RO}, {PLUGIN_ROLE_RW} TO {PLUGIN_ROLE_LOGIN}; END IF; END $$"
        ),
        // The plugin schema, owned by the write role so it may create its own
        // objects there. Read-only plugins only need USAGE.
        format!("CREATE SCHEMA IF NOT EXISTS {PLUGIN_SCHEMA} AUTHORIZATION {PLUGIN_ROLE_RW}"),
        format!("GRANT USAGE ON SCHEMA {PLUGIN_SCHEMA} TO {PLUGIN_ROLE_RO}"),
        // Both roles may read the HappyView schema's allowlisted tables (granted
        // below). Neither may create there — new plugin objects go to the plugin
        // schema — so CREATE is revoked in case an earlier boot granted it.
        format!("GRANT USAGE ON SCHEMA public TO {PLUGIN_ROLE_RO}, {PLUGIN_ROLE_RW}"),
        format!("REVOKE CREATE ON SCHEMA public FROM {PLUGIN_ROLE_RO}, {PLUGIN_ROLE_RW}"),
        // A table the write role creates in the plugin schema must be readable by
        // the read role (a `database:read` plugin runs under the read role).
        format!(
            "ALTER DEFAULT PRIVILEGES FOR ROLE {PLUGIN_ROLE_RW} IN SCHEMA {PLUGIN_SCHEMA} GRANT SELECT ON TABLES TO {PLUGIN_ROLE_RO}"
        ),
        // The plugin schema is first on the path so unqualified objects resolve
        // and are created there; `public` follows for the allowlisted tables and
        // existing caller tables. This path is set on the plugin roles only.
        // Set the path on the LOGIN role, where a role setting takes effect
        // (a `SET ROLE`'d NOLOGIN role never applies its own path — the
        // per-statement `SET LOCAL search_path` in `enter_plugin_role` is what
        // governs ro/rw). Harmless when the login role is absent.
        format!(
            "DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{PLUGIN_ROLE_LOGIN}') \
             THEN ALTER ROLE {PLUGIN_ROLE_LOGIN} SET search_path = {PLUGIN_SCHEMA}, public; END IF; END $$"
        ),
        // Existing caller-owned tables already in `public` are owned by the
        // HappyView role, so the restricted role cannot reach them without a
        // grant. Grant every non-`happyview_` relation in `public` (tables,
        // partitions and views) so a plugin keeps access to tables it created
        // before the schema existed. New caller tables go to the plugin schema.
        // Protected tables are excluded by prefix.
        format!(
            "DO $$ DECLARE r record; BEGIN \
             FOR r IN SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'public' AND c.relkind IN ('r','p','v') \
             AND c.relname NOT LIKE 'happyview\\_%' AND c.relname <> '_sqlx_migrations' LOOP \
             EXECUTE format('GRANT SELECT ON public.%I TO {PLUGIN_ROLE_RO}', r.relname); \
             EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON public.%I TO {PLUGIN_ROLE_RW}', r.relname); \
             END LOOP; END $$"
        ),
        // Sequences: only those a caller-owned table in `public` owns (a serial
        // column's, `a`, or an identity column's, `i`), found through the
        // sequence's dependency on its table rather than by name, since a
        // renamed table keeps its sequence's old name. Earlier grants are
        // revoked first so a sequence of a protected table never stays granted.
        format!(
            "REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM {PLUGIN_ROLE_RO}, {PLUGIN_ROLE_RW}"
        ),
        format!(
            "DO $$ DECLARE r record; BEGIN \
             FOR r IN SELECT s.relname FROM pg_class s \
             JOIN pg_namespace n ON n.oid = s.relnamespace \
             JOIN pg_depend d ON d.classid = 'pg_class'::regclass AND d.objid = s.oid \
               AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i') \
             JOIN pg_class t ON t.oid = d.refobjid \
             WHERE n.nspname = 'public' AND s.relkind = 'S' \
             AND t.relname NOT LIKE 'happyview\\_%' AND t.relname <> '_sqlx_migrations' LOOP \
             EXECUTE format('GRANT USAGE, SELECT ON SEQUENCE public.%I TO {PLUGIN_ROLE_RW}', r.relname); \
             END LOOP; END $$"
        ),
        // The plugin schema's own sequences, in case one there is not the write
        // role's (which owns what it creates).
        format!(
            "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA {PLUGIN_SCHEMA} TO {PLUGIN_ROLE_RW}"
        ),
    ];
    // The allowlisted internal tables: writes to the record views fire
    // `SECURITY DEFINER` triggers that reach the underlying tables, so the write
    // role needs no grant on those underlying tables.
    for rel in relations {
        stmts.push(format!("GRANT SELECT ON {rel} TO {PLUGIN_ROLE_RO}"));
        stmts.push(format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {rel} TO {PLUGIN_ROLE_RW}"
        ));
    }
    stmts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;
    use serde_json::json;
    use serial_test::serial;

    /// Every backend the suite can reach; the Postgres half needs a database
    /// to be pointed at and says so when there is none.
    async fn backends() -> Vec<(sqlx::AnyPool, DatabaseBackend)> {
        let mut out = vec![(
            crate::test_support::migrated_memory_pool().await,
            DatabaseBackend::Sqlite,
        )];
        match std::env::var("TEST_DATABASE_URL") {
            Ok(url) => {
                let backend = DatabaseBackend::from_url(&url);
                out.push((crate::db::connect(&url, backend).await, backend));
            }
            Err(_) => crate::test_support::unavailable(crate::test_support::NO_DATABASE),
        }
        out
    }

    /// Raw SQL is passed through untranslated, so a test writes each
    /// backend's own placeholder exactly as a plugin would.
    fn placeholder(backend: DatabaseBackend, position: usize) -> String {
        match backend {
            DatabaseBackend::Sqlite => "?".to_string(),
            DatabaseBackend::Postgres => format!("${position}"),
        }
    }

    /// A read routed to Postgres runs in a read-only transaction that is
    /// rolled back: it cannot write, and leaves no session state on the
    /// pooled connection for the next query to inherit.
    #[tokio::test]
    #[serial]
    async fn a_routed_read_on_postgres_can_neither_write_nor_change_the_session() {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            crate::test_support::unavailable(crate::test_support::NO_DATABASE);
            return;
        };
        let backend = DatabaseBackend::from_url(&url);
        if backend != DatabaseBackend::Postgres {
            return;
        }
        // One connection, so the second query runs where the first did.
        let db = crate::db::connect_with(&url, backend, 1).await;

        let rows = run_read_only_query_on(
            &db,
            backend,
            "SELECT set_config('application_name', 'changed by a read', false) AS v",
            &[],
        )
        .await
        .expect("the read runs");
        assert_eq!(rows[0]["v"], json!("changed by a read"));
        let rows = run_query(&db, "SELECT current_setting('application_name') AS v", &[])
            .await
            .unwrap();
        assert_ne!(rows[0]["v"], json!("changed by a read"));

        crate::db::query("CREATE SEQUENCE IF NOT EXISTS host_db_read_only_seq")
            .execute(&db)
            .await
            .unwrap();
        let err = run_read_only_query_on(
            &db,
            backend,
            "SELECT nextval('host_db_read_only_seq') AS v",
            &[],
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("read-only transaction"), "{err}");
    }

    /// A boolean parameter against a boolean column. Binding it as an integer
    /// is well-typed on SQLite, which has no boolean of its own, and leaves
    /// Postgres comparing `boolean = integer`, which it refuses.
    #[tokio::test]
    #[serial]
    async fn a_boolean_parameter_compares_against_a_boolean_column() {
        for (db, backend) in backends().await {
            crate::db::query(
                "CREATE TABLE IF NOT EXISTS host_db_flags (name TEXT, active BOOLEAN)",
            )
            .execute(&db)
            .await
            .expect("create the table");
            crate::db::query("DELETE FROM host_db_flags")
                .execute(&db)
                .await
                .expect("clear the table");

            let inserted = run_execute(
                &db,
                &format!(
                    "INSERT INTO host_db_flags (name, active) VALUES ({}, {}), ({}, {})",
                    placeholder(backend, 1),
                    placeholder(backend, 2),
                    placeholder(backend, 3),
                    placeholder(backend, 4),
                ),
                &[json!("on"), json!(true), json!("off"), json!(false)],
            )
            .await
            .expect("the insert should run");
            assert_eq!(inserted, 2);

            let rows = run_query(
                &db,
                &format!(
                    "SELECT name FROM host_db_flags WHERE active = {}",
                    placeholder(backend, 1)
                ),
                &[json!(true)],
            )
            .await
            .unwrap_or_else(|e| panic!("the query should run on {backend:?}: {e}"));
            assert_eq!(rows.len(), 1, "on {backend:?}");
            assert_eq!(rows[0]["name"], json!("on"));
        }
    }

    // -- Least-privileged plugin SQL (Postgres) -----------------------------
    //
    // These run the plugin raw-SQL entry points against a Postgres database and
    // assert that statements reaching HappyView's own internal tables are
    // refused, and that the role layer denies them at the database as well.

    use crate::raw_sql_guard::{PluginSqlEnforcement, PluginSqlMode};

    /// A Postgres pool with the least-privileged roles set up, or `None` (with a
    /// skip) when there is no Postgres to point at.
    async fn postgres_with_roles() -> Option<sqlx::AnyPool> {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            crate::test_support::unavailable(crate::test_support::NO_DATABASE);
            return None;
        };
        let backend = DatabaseBackend::from_url(&url);
        if backend != DatabaseBackend::Postgres {
            return None;
        }
        let db = crate::db::connect(&url, backend).await;
        assert!(
            ensure_plugin_roles(&db, backend).await,
            "the test database role should be able to set up the plugin roles"
        );
        Some(db)
    }

    /// Reads of internal tables that plugin SQL may not reach.
    const INTERNAL_READS: &[&str] = &[
        "SELECT * FROM happyview_users",
        "SELECT token_hash FROM happyview_api_keys",
        "SELECT * FROM happyview_oauth_sessions",
        "SELECT * FROM happyview_records_v2",
    ];

    #[tokio::test]
    #[serial]
    async fn plugin_reads_cannot_reach_internal_tables_on_postgres() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        for sql in INTERNAL_READS {
            // Both with the role layer and with only the parser allowlist.
            for enforcement in [PluginSqlEnforcement::Role, PluginSqlEnforcement::ParserOnly] {
                let err = run_plugin_query(
                    &db,
                    DatabaseBackend::Postgres,
                    enforcement,
                    PluginSqlMode::Read,
                    sql,
                    &[],
                )
                .await
                .expect_err(&format!(
                    "should be refused (enforcement={enforcement:?}): {sql}"
                ));
                assert!(!err.is_empty());
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn plugin_writes_cannot_reach_internal_tables_on_postgres() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        for sql in [
            "DO $$ BEGIN DELETE FROM happyview_users; END $$",
            "CREATE FUNCTION mischief() RETURNS int AS $$ SELECT count(*)::int FROM happyview_users $$ LANGUAGE sql",
            "UPDATE happyview_users SET id = id",
            "DELETE FROM happyview_api_keys",
        ] {
            for enforcement in [PluginSqlEnforcement::Role, PluginSqlEnforcement::ParserOnly] {
                let err = run_plugin_execute(&db, DatabaseBackend::Postgres, enforcement, sql, &[])
                    .await
                    .expect_err(&format!(
                        "should be refused (enforcement={enforcement:?}): {sql}"
                    ));
                assert!(!err.is_empty());
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn legitimate_plugin_sql_still_runs_on_postgres() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        // A read of an allowlisted table, under both the read and write role.
        for mode in [PluginSqlMode::Read, PluginSqlMode::Write] {
            run_plugin_query(
                &db,
                DatabaseBackend::Postgres,
                PluginSqlEnforcement::Role,
                mode,
                "SELECT count(*) AS n FROM happyview_records",
                &[],
            )
            .await
            .unwrap_or_else(|e| {
                panic!("a plain read of an allowlisted table should run ({mode:?}): {e}")
            });
        }
        // A write through the records compat view fires its INSTEAD OF trigger,
        // which runs as the write role — this proves the underlying-table grants
        // are in place. A URI that matches nothing changes no rows.
        run_plugin_execute(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::Role,
            "DELETE FROM happyview_records WHERE uri = $1",
            &[json!("at://did:plc:nobody/app.bsky.feed.post/none")],
        )
        .await
        .expect("a delete through the records view should run under the write role");
    }

    /// A plugin's own tables keep working under the write role: it can create
    /// one (needs CREATE on `public`), write and read it. Proves the documented
    /// caller-owned-table contract is preserved by the role layer.
    #[tokio::test]
    #[serial]
    async fn caller_owned_tables_work_under_the_write_role() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        // Clean up any leftover from a previous run (as the owning connection).
        crate::db::query("DROP TABLE IF EXISTS happyview_plugin.plugin_caller_tbl")
            .execute(&db)
            .await
            .unwrap();

        run_plugin_execute(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::Role,
            "CREATE TABLE plugin_caller_tbl (k TEXT, v TEXT)",
            &[],
        )
        .await
        .expect("the write role can create its own table");
        run_plugin_execute(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::Role,
            "INSERT INTO plugin_caller_tbl (k, v) VALUES ($1, $2)",
            &[json!("a"), json!("1")],
        )
        .await
        .expect("the write role can write its own table");
        let rows = run_plugin_query(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::Role,
            PluginSqlMode::Write,
            "SELECT v FROM plugin_caller_tbl WHERE k = $1",
            &[json!("a")],
        )
        .await
        .expect("the write role can read its own table");
        assert_eq!(rows[0]["v"], json!("1"));

        run_plugin_execute(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::Role,
            "DROP TABLE plugin_caller_tbl",
            &[],
        )
        .await
        .expect("the write role can drop its own table");
    }

    /// A table the write role creates is readable by the read role, via the
    /// default privileges set up at boot — so a `database:read` plugin can read
    /// a table a `database:write` plugin created.
    #[tokio::test]
    #[serial]
    async fn a_read_role_reads_a_table_the_write_role_created() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        crate::db::query("DROP TABLE IF EXISTS happyview_plugin.plugin_shared_tbl")
            .execute(&db)
            .await
            .unwrap();
        run_plugin_execute(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::Role,
            "CREATE TABLE plugin_shared_tbl (k TEXT)",
            &[],
        )
        .await
        .expect("write role creates the table");
        run_plugin_execute(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::Role,
            "INSERT INTO plugin_shared_tbl (k) VALUES ($1)",
            &[json!("v")],
        )
        .await
        .unwrap();

        // The read role (database:read) reads it.
        let rows = run_plugin_query(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::Role,
            PluginSqlMode::Read,
            "SELECT k FROM plugin_shared_tbl",
            &[],
        )
        .await
        .expect("the read role should read a table the write role created");
        assert_eq!(rows[0]["k"], json!("v"));

        crate::db::query("DROP TABLE IF EXISTS happyview_plugin.plugin_shared_tbl")
            .execute(&db)
            .await
            .unwrap();
    }

    /// Proves the role is a real database boundary, not only a parser check: the
    /// read role has no access to an internal table, so a read of one under that
    /// role is denied by the server regardless of how the statement is written.
    /// This is what the parser's allowlist falls back on.
    #[tokio::test]
    #[serial]
    async fn the_read_role_cannot_read_an_internal_table() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        let mut tx = db.begin().await.unwrap();
        crate::db::query(&format!("SET LOCAL ROLE {PLUGIN_ROLE_RO}"))
            .execute(&mut *tx)
            .await
            .unwrap();
        let msg = match crate::db::query("SELECT * FROM happyview_users")
            .fetch_all(&mut *tx)
            .await
        {
            Ok(_) => panic!("the read role must be denied this read at the database"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("permission denied") || msg.contains("denied"),
            "expected a permission error, got: {msg}"
        );
        let _ = tx.rollback().await;
    }

    /// Under the role layer, a plugin statement runs as the plugin role, not the
    /// privileged connecting user, and cannot switch back.
    #[tokio::test]
    #[serial]
    async fn plugin_statements_run_as_the_plugin_role() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        let read = run_plugin_query(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::Role,
            PluginSqlMode::Read,
            "SELECT current_user::text AS u",
            &[],
        )
        .await
        .expect("read runs");
        assert_eq!(read[0]["u"], json!(PLUGIN_ROLE_RO));

        let write = run_plugin_query(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::Role,
            PluginSqlMode::Write,
            "SELECT current_user::text AS u",
            &[],
        )
        .await
        .expect("write-mode read runs");
        assert_eq!(write[0]["u"], json!(PLUGIN_ROLE_RW));
    }

    /// A plugin cannot take a session advisory lock (which would outlive its
    /// statement and could block HappyView's own locks): the deny-list refuses
    /// the function in every mode.
    #[tokio::test]
    #[serial]
    async fn a_plugin_cannot_hold_a_session_advisory_lock() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        for enforcement in [PluginSqlEnforcement::Role, PluginSqlEnforcement::ParserOnly] {
            let err = run_plugin_query(
                &db,
                DatabaseBackend::Postgres,
                enforcement,
                PluginSqlMode::Read,
                "SELECT pg_advisory_lock(1)",
                &[],
            )
            .await
            .expect_err(&format!(
                "advisory lock should be refused ({enforcement:?})"
            ));
            assert!(!err.is_empty());
        }
        // Nothing held the lock, so it is free.
        let rows = run_plugin_query(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::Role,
            PluginSqlMode::Read,
            "SELECT count(*) AS n FROM happyview_records",
            &[],
        )
        .await
        .expect("a later statement still runs");
        assert!(rows[0]["n"].is_number());
    }

    /// The login pool connects as the unprivileged `happyview_plugin` role, so
    /// `current_user` is that role (not a privileged user), and a plugin can
    /// define its own function in its schema. Builds its own login pool.
    #[tokio::test]
    #[serial]
    async fn login_pool_runs_as_the_login_role_and_allows_plugin_objects() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            return;
        };
        // Create the login role (provisioned by infra in production), give it a
        // password, and grant it the bundles, then build a URL for it.
        crate::db::query(&format!(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{PLUGIN_ROLE_LOGIN}') \
             THEN CREATE ROLE {PLUGIN_ROLE_LOGIN} LOGIN PASSWORD 'plugin_test_pw'; \
             ELSE ALTER ROLE {PLUGIN_ROLE_LOGIN} LOGIN PASSWORD 'plugin_test_pw'; END IF; END $$"
        ))
        .execute(&db)
        .await
        .expect("make the login role usable");
        crate::db::query(&format!(
            "GRANT {PLUGIN_ROLE_RO}, {PLUGIN_ROLE_RW} TO {PLUGIN_ROLE_LOGIN}"
        ))
        .execute(&db)
        .await
        .expect("grant the bundles to the login role");
        let login_url = url
            .replacen(
                "//happyview:happyview@",
                "//happyview_plugin:plugin_test_pw@",
                1,
            )
            .replacen("//happyview@", "//happyview_plugin:plugin_test_pw@", 1);
        let Some(login_pool) = open_plugin_login_pool(&login_url, "happyview").await else {
            // The test database may not allow password auth for the role; skip.
            crate::db::query(&format!("ALTER ROLE {PLUGIN_ROLE_LOGIN} NOLOGIN"))
                .execute(&db)
                .await
                .ok();
            return;
        };

        // The reset runs as the login role itself, and a connection it reset
        // keeps serving the same statements.
        let single = one_connection_login_pool(&login_url).await;
        for _ in 0..3 {
            let rows = run_plugin_query(
                &single,
                DatabaseBackend::Postgres,
                PluginSqlEnforcement::LoginPool,
                PluginSqlMode::Write,
                "SELECT session_user::text AS s, current_user::text AS c",
                &[],
            )
            .await
            .expect("a reset login-role connection runs the statement again");
            assert_eq!(rows[0]["s"], json!(PLUGIN_ROLE_LOGIN));
        }
        single.close().await;

        // `session_user` is the unprivileged login role (so `RESET ROLE` lands
        // there, not on a privileged user); `current_user` is the per-statement
        // role from `SET LOCAL ROLE`.
        let who = run_plugin_query(
            &login_pool,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::LoginPool,
            PluginSqlMode::Write,
            "SELECT session_user::text AS s, current_user::text AS c",
            &[],
        )
        .await
        .expect("runs on the login pool");
        assert_eq!(who[0]["s"], json!(PLUGIN_ROLE_LOGIN));
        assert_eq!(who[0]["c"], json!(PLUGIN_ROLE_RW));

        // A statement may not change the role it runs under, through either
        // entry point, and the next statement still runs as the same roles.
        for sql in [
            "RESET ROLE",
            "SET ROLE NONE",
            "SET SESSION AUTHORIZATION DEFAULT",
        ] {
            for mode in [PluginSqlMode::Read, PluginSqlMode::Write] {
                assert!(
                    run_plugin_query(
                        &login_pool,
                        DatabaseBackend::Postgres,
                        PluginSqlEnforcement::LoginPool,
                        mode,
                        sql,
                        &[],
                    )
                    .await
                    .is_err(),
                    "the login pool must refuse {sql} ({mode:?})"
                );
            }
            assert!(
                run_plugin_execute(
                    &login_pool,
                    DatabaseBackend::Postgres,
                    PluginSqlEnforcement::LoginPool,
                    sql,
                    &[],
                )
                .await
                .is_err(),
                "the login pool must refuse {sql} as an execute"
            );
        }
        let again = run_plugin_query(
            &login_pool,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::LoginPool,
            PluginSqlMode::Write,
            "SELECT session_user::text AS s, current_user::text AS c",
            &[],
        )
        .await
        .expect("runs on the login pool");
        assert_eq!(again, who);

        // A plugin defines its own function in its schema, then calls it.
        run_plugin_execute(
            &login_pool,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::LoginPool,
            "CREATE OR REPLACE FUNCTION plugin_double(n int) RETURNS int LANGUAGE sql AS $$ SELECT n * 2 $$",
            &[],
        )
        .await
        .expect("a plugin may define a function in its schema");

        clear_plugin_schema_code(&db).await;
        drop(login_pool);

        // A membership in any role besides the two plugin roles refuses the pool:
        // a granted role could carry privileges the checks above don't sample.
        for stmt in [
            "DROP ROLE IF EXISTS hv_test_extra_role".to_string(),
            "CREATE ROLE hv_test_extra_role NOLOGIN".to_string(),
            format!("GRANT hv_test_extra_role TO {PLUGIN_ROLE_LOGIN}"),
        ] {
            crate::db::query(&stmt).execute(&db).await.unwrap();
        }
        let with_extra = open_plugin_login_pool(&login_url, "happyview").await;
        crate::db::query("DROP ROLE hv_test_extra_role")
            .execute(&db)
            .await
            .unwrap();
        assert!(
            with_extra.is_none(),
            "a login role that is a member of another role must be refused"
        );

        crate::db::query(&format!("ALTER ROLE {PLUGIN_ROLE_LOGIN} NOLOGIN"))
            .execute(&db)
            .await
            .ok();
    }

    /// The login-pool role is verified, not trusted: pointing it at the main
    /// (privileged) database role is refused.
    #[tokio::test]
    #[serial]
    async fn the_main_role_is_refused_as_a_login_pool() {
        let Some(_db) = postgres_with_roles().await else {
            return;
        };
        let url = std::env::var("TEST_DATABASE_URL").unwrap();
        // `main_role` here is the very role the URL logs in as, so the identity
        // check alone refuses it (it is also privileged and can read secrets).
        assert!(
            open_plugin_login_pool(&url, "happyview").await.is_none(),
            "the main database role must be refused as a login pool"
        );
    }

    /// Code a plugin left in its schema is detected, so boot can refuse plugin
    /// SQL outside the login pool rather than run it with the privileged user.
    #[tokio::test]
    #[serial]
    async fn plugin_schema_code_is_detected() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        // Clear any routines other tests left in the shared schema, so "empty"
        // is actually empty.
        crate::db::query(
            "DO $$ DECLARE r record; BEGIN \
             FOR r IN SELECT p.oid::regprocedure AS sig FROM pg_proc p \
               JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'happyview_plugin' \
             LOOP EXECUTE 'DROP FUNCTION ' || r.sig; END LOOP; END $$",
        )
        .execute(&db)
        .await
        .unwrap();
        assert!(
            !plugin_schema_has_code(&db, DatabaseBackend::Postgres).await,
            "an empty plugin schema has no code"
        );
        crate::db::query(
            "CREATE FUNCTION happyview_plugin.left_behind() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$",
        )
        .execute(&db)
        .await
        .unwrap();
        assert!(
            plugin_schema_has_code(&db, DatabaseBackend::Postgres).await,
            "a function in the plugin schema is detected"
        );
        crate::db::query("DROP FUNCTION happyview_plugin.left_behind()")
            .execute(&db)
            .await
            .unwrap();
    }

    /// The write role cannot create in `public` — the "never on HappyView's
    /// search path" guarantee. New objects go to the plugin schema instead.
    #[tokio::test]
    #[serial]
    async fn the_write_role_cannot_create_in_public() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        let mut tx = db.begin().await.unwrap();
        crate::db::query(&format!("SET LOCAL ROLE {PLUGIN_ROLE_RW}"))
            .execute(&mut *tx)
            .await
            .unwrap();
        let err = crate::db::query("CREATE TABLE public.plugin_should_not_exist (a int)")
            .execute(&mut *tx)
            .await
            .expect_err("the write role must not create in public");
        assert!(err.to_string().contains("denied"), "{err}");
        let _ = tx.rollback().await;
    }

    /// A read through `run_plugin_query` runs in a read-only transaction. A
    /// plain SELECT may call a function that writes (the login pool lets a
    /// plugin define one), and the read-only transaction is what stops it: the
    /// error is "read-only", not the read role's missing grant, which would be
    /// all that stood in the way without it.
    #[tokio::test]
    #[serial]
    async fn a_plugin_read_cannot_write_through_a_function() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        crate::db::query(
            "CREATE OR REPLACE FUNCTION public.hv_test_read_probe() RETURNS int LANGUAGE sql \
             AS $$ DELETE FROM happyview_jobs WHERE false; SELECT 1 $$",
        )
        .execute(&db)
        .await
        .unwrap();
        let result = run_plugin_query(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::LoginPool,
            PluginSqlMode::Read,
            "SELECT hv_test_read_probe() AS n",
            &[],
        )
        .await;
        crate::db::query("DROP FUNCTION public.hv_test_read_probe()")
            .execute(&db)
            .await
            .unwrap();
        let err = result.expect_err("a read must not write through a function");
        assert!(
            err.contains("read-only") || err.contains("read only"),
            "expected a read-only error, got: {err}"
        );
    }

    /// In parser-only mode the privileged connection does not search the plugin
    /// schema, so an unqualified name resolves to `public` even when the plugin
    /// schema holds an object of the same name.
    #[tokio::test]
    #[serial]
    async fn parser_only_resolves_names_outside_the_plugin_schema() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        for stmt in [
            "DROP TABLE IF EXISTS public.hv_test_path_probe",
            "DROP TABLE IF EXISTS happyview_plugin.hv_test_path_probe",
            "CREATE TABLE public.hv_test_path_probe (v text)",
            "INSERT INTO public.hv_test_path_probe VALUES ('public')",
            "CREATE TABLE happyview_plugin.hv_test_path_probe (v text)",
            "INSERT INTO happyview_plugin.hv_test_path_probe VALUES ('plugin')",
        ] {
            crate::db::query(stmt).execute(&db).await.unwrap();
        }
        let result = run_plugin_query(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::ParserOnly,
            PluginSqlMode::Read,
            "SELECT v FROM hv_test_path_probe",
            &[],
        )
        .await;
        for stmt in [
            "DROP TABLE public.hv_test_path_probe",
            "DROP TABLE happyview_plugin.hv_test_path_probe",
        ] {
            crate::db::query(stmt).execute(&db).await.unwrap();
        }
        let rows = result.expect("parser-only read of a caller table");
        assert_eq!(rows[0]["v"], json!("public"));
    }

    /// The write role holds no privilege on a sequence a protected table owns,
    /// whatever the sequence is named, so it cannot advance one; a caller-owned
    /// table's sequence stays usable.
    #[tokio::test]
    #[serial]
    async fn the_write_role_reaches_only_caller_table_sequences() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        crate::db::query("DROP TABLE IF EXISTS public.hv_test_serial_caller")
            .execute(&db)
            .await
            .unwrap();
        crate::db::query("CREATE TABLE public.hv_test_serial_caller (id serial, v text)")
            .execute(&db)
            .await
            .unwrap();
        assert!(ensure_plugin_roles(&db, DatabaseBackend::Postgres).await);

        let protected: Vec<String> = crate::db::query(&format!(
            "SELECT s.relname::text AS seq FROM pg_class s \
             JOIN pg_namespace n ON n.oid = s.relnamespace \
             JOIN pg_depend d ON d.classid = 'pg_class'::regclass AND d.objid = s.oid \
               AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i') \
             JOIN pg_class t ON t.oid = d.refobjid \
             WHERE n.nspname = 'public' AND s.relkind = 'S' AND t.relname LIKE 'happyview\\_%' \
             AND (has_sequence_privilege('{PLUGIN_ROLE_RW}', s.oid, 'USAGE') \
               OR has_sequence_privilege('{PLUGIN_ROLE_RW}', s.oid, 'SELECT') \
               OR has_sequence_privilege('{PLUGIN_ROLE_RW}', s.oid, 'UPDATE'))"
        ))
        .fetch_all(&db)
        .await
        .unwrap()
        .iter()
        .map(|r| r.try_get::<String, _>("seq").unwrap())
        .collect();
        assert!(
            protected.is_empty(),
            "the write role holds privileges on protected sequences: {protected:?}"
        );

        let mut tx = db.begin().await.unwrap();
        crate::db::query(&format!("SET LOCAL ROLE {PLUGIN_ROLE_RW}"))
            .execute(&mut *tx)
            .await
            .unwrap();
        let err = crate::db::query("SELECT nextval('happyview_collections_id_seq')")
            .execute(&mut *tx)
            .await
            .expect_err("the write role must not advance a protected sequence");
        assert!(err.to_string().contains("denied"), "{err}");
        let _ = tx.rollback().await;

        let inserted = run_plugin_execute(
            &db,
            DatabaseBackend::Postgres,
            PluginSqlEnforcement::Role,
            "INSERT INTO hv_test_serial_caller (v) VALUES ('x')",
            &[],
        )
        .await;
        crate::db::query("DROP TABLE public.hv_test_serial_caller")
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(inserted.expect("a caller table's serial column works"), 1);
    }

    /// Drop every routine and trigger in the plugin schema, so a test that needs
    /// it free of code starts from there.
    async fn clear_plugin_schema_code(db: &sqlx::AnyPool) {
        crate::db::query(
            "DO $$ DECLARE r record; BEGIN \
             FOR r IN SELECT t.tgname, c.oid::regclass AS rel FROM pg_trigger t \
               JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
               WHERE n.nspname = 'happyview_plugin' AND NOT t.tgisinternal \
             LOOP EXECUTE format('DROP TRIGGER %I ON %s', r.tgname, r.rel); END LOOP; \
             FOR r IN SELECT p.oid::regprocedure AS sig FROM pg_proc p \
               JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'happyview_plugin' \
             LOOP EXECUTE 'DROP ROUTINE ' || r.sig; END LOOP; END $$",
        )
        .execute(db)
        .await
        .unwrap();
    }

    /// A login URL that is set but cannot be used refuses plugin SQL rather
    /// than falling back to a weaker mode: one naming the privileged main role
    /// (fails the role check) and one that cannot connect.
    #[tokio::test]
    #[serial]
    async fn a_set_but_unusable_login_url_refuses_plugin_sql() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        clear_plugin_schema_code(&db).await;
        let main_url = std::env::var("TEST_DATABASE_URL").unwrap();
        for url in [
            main_url.as_str(),
            "postgres://nobody:nothing@127.0.0.1:1/none",
        ] {
            let access = decide_plugin_sql_access(&db, DatabaseBackend::Postgres, Some(url)).await;
            assert!(
                matches!(access, crate::PluginSqlAccess::Refused),
                "an unusable login URL must refuse plugin SQL: {url}"
            );
        }
        // Without the URL the same database runs in role mode, so the refusals
        // above came from the URL.
        let access = decide_plugin_sql_access(&db, DatabaseBackend::Postgres, None).await;
        assert!(matches!(access, crate::PluginSqlAccess::Role));
    }

    /// With no login URL, code in the plugin schema refuses plugin SQL: it
    /// would run with the privileged session user.
    #[tokio::test]
    #[serial]
    async fn plugin_schema_code_without_a_login_url_refuses_plugin_sql() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        clear_plugin_schema_code(&db).await;
        crate::db::query(
            "CREATE FUNCTION happyview_plugin.left_behind() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$",
        )
        .execute(&db)
        .await
        .unwrap();
        let refused = decide_plugin_sql_access(&db, DatabaseBackend::Postgres, None).await;
        clear_plugin_schema_code(&db).await;
        assert!(matches!(refused, crate::PluginSqlAccess::Refused));
        let cleared = decide_plugin_sql_access(&db, DatabaseBackend::Postgres, None).await;
        assert!(
            matches!(cleared, crate::PluginSqlAccess::Role),
            "with the code dropped, plugin SQL runs in role mode again"
        );
    }

    /// A one-connection login pool, so release and re-acquire reuse it. Connects
    /// as `url`'s role; the role checks are not what these tests are about.
    async fn one_connection_login_pool(url: &str) -> sqlx::AnyPool {
        crate::db::install_drivers();
        plugin_login_pool_options()
            .max_connections(1)
            .connect(url)
            .await
            .unwrap()
    }

    /// A reused login-pool connection keeps nothing a statement left on the
    /// session: settings, a session-level role and path, a temporary table, an
    /// advisory lock and an open cursor are all gone after release.
    #[tokio::test]
    #[serial]
    async fn a_released_login_pool_connection_keeps_no_session_state() {
        let Some(_roles) = postgres_with_roles().await else {
            return;
        };
        let url = std::env::var("TEST_DATABASE_URL").unwrap();
        let pool = one_connection_login_pool(&url).await;
        let before: (String, String) =
            sqlx::query_as("SELECT current_user::text, current_setting('search_path')")
                .fetch_one(&pool)
                .await
                .unwrap();
        {
            let mut c = pool.acquire().await.unwrap();
            for stmt in [
                "SET application_name = 'left_behind'".to_string(),
                "SET search_path = pg_catalog".to_string(),
                "CREATE TEMP TABLE left_behind_temp (a int)".to_string(),
                "SELECT pg_advisory_lock(424242)".to_string(),
                "DECLARE left_behind_cursor CURSOR WITH HOLD FOR SELECT 1".to_string(),
                format!("SET ROLE {PLUGIN_ROLE_RW}"),
            ] {
                sqlx::raw_sql(sqlx::AssertSqlSafe(stmt.clone()))
                    .execute(&mut *c)
                    .await
                    .unwrap_or_else(|e| panic!("{stmt}: {e}"));
            }
        } // released here: the reset runs

        let mut c = pool.acquire().await.unwrap();
        let row: (String, String, String, Option<String>, i64, i64) = sqlx::query_as(
            "SELECT current_user::text, current_setting('search_path'), \
             current_setting('application_name'), to_regclass('pg_temp.left_behind_temp')::text, \
             (SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND pid = pg_backend_pid()), \
             (SELECT count(*) FROM pg_cursors WHERE name = 'left_behind_cursor')",
        )
        .fetch_one(&mut *c)
        .await
        .unwrap();
        assert_eq!(row.0, before.0, "the role is reset");
        assert_eq!(row.1, before.1, "the search_path is reset");
        assert_ne!(row.2, "left_behind", "session settings are reset");
        assert_eq!(row.3, None, "temporary objects are dropped");
        assert_eq!(row.4, 0, "advisory locks are released");
        assert_eq!(row.5, 0, "cursors are closed");
    }

    /// A login-pool connection serves statement after statement: the reset on
    /// release keeps the statements the driver has prepared on it. Each plugin
    /// statement prepares the same role and path statements, so a reset that
    /// dropped them would fail on the next caller.
    #[tokio::test]
    #[serial]
    async fn a_reused_login_pool_connection_runs_the_same_statement_again() {
        let Some(_roles) = postgres_with_roles().await else {
            return;
        };
        let url = std::env::var("TEST_DATABASE_URL").unwrap();
        let pool = one_connection_login_pool(&url).await;
        for mode in [
            PluginSqlMode::Read,
            PluginSqlMode::Write,
            PluginSqlMode::Read,
        ] {
            run_plugin_query(
                &pool,
                DatabaseBackend::Postgres,
                PluginSqlEnforcement::LoginPool,
                mode,
                "SELECT count(*) AS n FROM happyview_records",
                &[],
            )
            .await
            .expect("a reused connection runs the statement again");
        }
    }
}
