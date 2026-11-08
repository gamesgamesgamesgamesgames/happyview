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
// On Postgres the tokenizer table guard is not the boundary: SQL hidden in a
// string handed to a built-in (`query_to_xml`, …) never names the table as an
// identifier. Two things close that:
//
//  1. `raw_sql_guard::check_postgres_plugin_sql` parses with Postgres's own
//     parser and accepts only allowlisted statement kinds, relations and
//     functions.
//  2. the statement runs under a role (`happyview_plugin_ro`/`_rw`) GRANTed only
//     the allowlisted relations, so even a function that executes a string runs
//     as that role and the server denies anything off the allowlist.
//
// (1) is applied always; (2) when the roles could be set up at boot. When they
// could not, (1) is the whole boundary and the statement runs on the privileged
// connection, still in a read-only transaction for reads. SQLite keeps the
// tokenizer guard and the plain run paths — it has no string-executing function
// to smuggle a table through.

/// The least-privileged role a read-only plugin statement runs under.
pub const PLUGIN_ROLE_RO: &str = "happyview_plugin_ro";
/// The least-privileged role a writing plugin statement runs under.
pub const PLUGIN_ROLE_RW: &str = "happyview_plugin_rw";

/// Run a plugin query on the main pool under the least-privileged role when
/// available. `mode` picks the role and, for a read, a rolled-back read-only
/// transaction. SQLite keeps [`run_query_on`].
pub async fn run_plugin_query(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    restricted: bool,
    mode: crate::raw_sql_guard::PluginSqlMode,
    sql: &str,
    params: &[Value],
) -> Result<Vec<Map<String, Value>>, String> {
    use crate::raw_sql_guard::PluginSqlMode;
    if backend == crate::db::DatabaseBackend::Sqlite {
        return run_query(db, sql, params).await;
    }
    crate::raw_sql_guard::check_postgres_plugin_sql(sql, mode, restricted)?;
    let role = match mode {
        PluginSqlMode::Read => PLUGIN_ROLE_RO,
        PluginSqlMode::Write => PLUGIN_ROLE_RW,
    };
    let read_only = mode == PluginSqlMode::Read;
    let query = bind_params(crate::db::query(sql), params)?;
    let mut tx = db.begin().await.map_err(|e| format!("query failed: {e}"))?;
    enter_plugin_role(&mut tx, role, restricted, read_only).await?;
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

/// Run a plugin write on the main pool under `happyview_plugin_rw` when
/// available, committing on success. SQLite keeps [`run_execute`].
pub async fn run_plugin_execute(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    restricted: bool,
    sql: &str,
    params: &[Value],
) -> Result<u64, String> {
    if backend == crate::db::DatabaseBackend::Sqlite {
        return run_execute(db, sql, params).await;
    }
    crate::raw_sql_guard::check_postgres_plugin_sql(
        sql,
        crate::raw_sql_guard::PluginSqlMode::Write,
        restricted,
    )?;
    let query = bind_params(crate::db::query(sql), params)?;
    let mut tx = db
        .begin()
        .await
        .map_err(|e| format!("execute failed: {e}"))?;
    enter_plugin_role(&mut tx, PLUGIN_ROLE_RW, restricted, false).await?;
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
/// role (when set up) and pin `search_path`, both with `SET LOCAL` so neither
/// leaks onto the pooled connection the next caller reuses. For a read, also
/// mark the transaction read-only before any query runs.
async fn enter_plugin_role(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    role: &str,
    restricted: bool,
    read_only: bool,
) -> Result<(), String> {
    if read_only {
        crate::db::query("SET TRANSACTION READ ONLY")
            .execute(&mut **tx)
            .await
            .map_err(|e| format!("query failed: {e}"))?;
    }
    if restricted {
        // `role` is one of our own constants, never caller input.
        crate::db::query(&format!("SET LOCAL ROLE {role}"))
            .execute(&mut **tx)
            .await
            .map_err(|e| format!("query failed: {e}"))?;
    }
    crate::db::query("SET LOCAL search_path = public")
        .execute(&mut **tx)
        .await
        .map_err(|e| format!("query failed: {e}"))?;
    Ok(())
}

/// Ensure the two least-privileged plugin roles exist and hold grants on exactly
/// the currently-allowlisted relations, and that the connecting role may
/// `SET ROLE` to them. Postgres only; returns whether plugin SQL can run under
/// them. Idempotent and safe to run at every boot — the `REVOKE` before the
/// `GRANT`s is what lets a relation dropped from the allowlist lose access.
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

/// The idempotent statements that create and (re)grant the plugin roles. Kept
/// as data so a test can read the grant targets back and pin them against the
/// allowlist.
fn plugin_role_setup_statements() -> Vec<String> {
    let relations = crate::raw_sql_guard::plugin_nameable_relations();
    // The real tables behind the record compat views: writes through the views
    // fire `INSTEAD OF` triggers that run as the writing role, so the write role
    // needs access to the underlying rows as well. Reads go through the views'
    // owner rights and need nothing here.
    const RECORD_UNDERLYING: &[&str] = &[
        "happyview_records_v2",
        "happyview_record_refs_v2",
        "happyview_dids",
        "happyview_collections",
    ];

    let mut stmts = vec![
        format!(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{PLUGIN_ROLE_RO}') THEN CREATE ROLE {PLUGIN_ROLE_RO} NOLOGIN; END IF; END $$"
        ),
        format!(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{PLUGIN_ROLE_RW}') THEN CREATE ROLE {PLUGIN_ROLE_RW} NOLOGIN; END IF; END $$"
        ),
        format!("GRANT {PLUGIN_ROLE_RO}, {PLUGIN_ROLE_RW} TO CURRENT_USER"),
        // Re-sync from a clean slate so a relation removed from the allowlist is
        // actually revoked.
        format!("REVOKE ALL ON ALL TABLES IN SCHEMA public FROM {PLUGIN_ROLE_RO}"),
        format!("REVOKE ALL ON ALL TABLES IN SCHEMA public FROM {PLUGIN_ROLE_RW}"),
        format!("GRANT USAGE ON SCHEMA public TO {PLUGIN_ROLE_RO}, {PLUGIN_ROLE_RW}"),
        // The write role may create and manage its own tables in `public`
        // (plugins are expected to use their own tables through raw SQL), so it
        // needs CREATE. The read role does not.
        format!("GRANT CREATE ON SCHEMA public TO {PLUGIN_ROLE_RW}"),
        // A table the write role creates later must be readable by the read
        // role (a `database:read` plugin runs under the read role). Default
        // privileges apply to objects the write role creates from now on.
        format!(
            "ALTER DEFAULT PRIVILEGES FOR ROLE {PLUGIN_ROLE_RW} IN SCHEMA public GRANT SELECT ON TABLES TO {PLUGIN_ROLE_RO}"
        ),
        format!("ALTER ROLE {PLUGIN_ROLE_RO} SET search_path = public"),
        format!("ALTER ROLE {PLUGIN_ROLE_RW} SET search_path = public"),
        // Writes may need sequence counters; sequences hold no data of concern.
        format!("GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO {PLUGIN_ROLE_RW}"),
        // Caller-owned tables already in `public` are owned by the HappyView
        // role, so the restricted role cannot reach them without a grant. Grant
        // every non-`happyview_` relation in `public` (tables, partitions and
        // views) so a plugin keeps access to tables it created before the roles
        // existed. Future caller tables are created by the write role itself and
        // need nothing here. Protected internal tables are excluded by prefix.
        format!(
            "DO $$ DECLARE r record; BEGIN \
             FOR r IN SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'public' AND c.relkind IN ('r','p','v') \
             AND c.relname NOT LIKE 'happyview\\_%' AND c.relname <> '_sqlx_migrations' LOOP \
             EXECUTE format('GRANT SELECT ON public.%I TO {PLUGIN_ROLE_RO}', r.relname); \
             EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON public.%I TO {PLUGIN_ROLE_RW}', r.relname); \
             END LOOP; END $$"
        ),
    ];
    for rel in relations {
        stmts.push(format!("GRANT SELECT ON {rel} TO {PLUGIN_ROLE_RO}"));
        stmts.push(format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {rel} TO {PLUGIN_ROLE_RW}"
        ));
    }
    for rel in RECORD_UNDERLYING {
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
    // refused — both those naming a table directly and those that reach one
    // through a string-taking built-in. On the old tokenizer guard the latter
    // returned rows; here they are refused by the parser allowlist, and the
    // role layer denies them at the database as well.

    use crate::raw_sql_guard::PluginSqlMode;

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

    /// Statements that reach an internal table that plugin SQL may not. Each
    /// reproduces a way the tokenizer guard could be slipped: a built-in that
    /// runs or resolves a string, string concatenation, a dollar-quoted body.
    const BYPASS_READS: &[&str] = &[
        "SELECT query_to_xml('select * from happyview_users', true, false, '')",
        "SELECT table_to_xml('happyview_users'::regclass, true, false, '')",
        "SELECT query_to_xml($q$select * from happyview_api_keys$q$, true, false, '')",
        "SELECT * FROM happyview_users",
        "SELECT * FROM happyview_oauth_sessions",
    ];

    #[tokio::test]
    #[serial]
    async fn plugin_reads_cannot_reach_internal_tables_on_postgres() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        for sql in BYPASS_READS {
            // Both with the role layer and with only the parser allowlist.
            for restricted in [true, false] {
                let err = run_plugin_query(
                    &db,
                    DatabaseBackend::Postgres,
                    restricted,
                    PluginSqlMode::Read,
                    sql,
                    &[],
                )
                .await
                .expect_err(&format!(
                    "should be refused (restricted={restricted}): {sql}"
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
            "CREATE FUNCTION pwn() RETURNS int AS $$ SELECT count(*)::int FROM happyview_users $$ LANGUAGE sql",
            "UPDATE happyview_users SET id = id",
            "DELETE FROM happyview_api_keys",
        ] {
            for restricted in [true, false] {
                let err = run_plugin_execute(&db, DatabaseBackend::Postgres, restricted, sql, &[])
                    .await
                    .expect_err(&format!(
                        "should be refused (restricted={restricted}): {sql}"
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
                true,
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
            true,
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
        crate::db::query("DROP TABLE IF EXISTS hv393_caller_state")
            .execute(&db)
            .await
            .unwrap();

        run_plugin_execute(
            &db,
            DatabaseBackend::Postgres,
            true,
            "CREATE TABLE hv393_caller_state (k TEXT, v TEXT)",
            &[],
        )
        .await
        .expect("the write role can create its own table");
        run_plugin_execute(
            &db,
            DatabaseBackend::Postgres,
            true,
            "INSERT INTO hv393_caller_state (k, v) VALUES ($1, $2)",
            &[json!("a"), json!("1")],
        )
        .await
        .expect("the write role can write its own table");
        let rows = run_plugin_query(
            &db,
            DatabaseBackend::Postgres,
            true,
            PluginSqlMode::Write,
            "SELECT v FROM hv393_caller_state WHERE k = $1",
            &[json!("a")],
        )
        .await
        .expect("the write role can read its own table");
        assert_eq!(rows[0]["v"], json!("1"));

        run_plugin_execute(
            &db,
            DatabaseBackend::Postgres,
            true,
            "DROP TABLE hv393_caller_state",
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
        crate::db::query("DROP TABLE IF EXISTS hv393_shared_state")
            .execute(&db)
            .await
            .unwrap();
        run_plugin_execute(
            &db,
            DatabaseBackend::Postgres,
            true,
            "CREATE TABLE hv393_shared_state (k TEXT)",
            &[],
        )
        .await
        .expect("write role creates the table");
        run_plugin_execute(
            &db,
            DatabaseBackend::Postgres,
            true,
            "INSERT INTO hv393_shared_state (k) VALUES ($1)",
            &[json!("v")],
        )
        .await
        .unwrap();

        // The read role (database:read) reads it.
        let rows = run_plugin_query(
            &db,
            DatabaseBackend::Postgres,
            true,
            PluginSqlMode::Read,
            "SELECT k FROM hv393_shared_state",
            &[],
        )
        .await
        .expect("the read role should read a table the write role created");
        assert_eq!(rows[0]["k"], json!("v"));

        crate::db::query("DROP TABLE IF EXISTS hv393_shared_state")
            .execute(&db)
            .await
            .unwrap();
    }

    /// Proves the role is a real database boundary, not only a parser check: a
    /// string-executing built-in run *under the read role* is denied by the
    /// server even though, bypassing our parser, it names the table only in a
    /// string. This is what the parser allowlist falls back on.
    #[tokio::test]
    #[serial]
    async fn the_read_role_denies_a_string_executed_internal_read() {
        let Some(db) = postgres_with_roles().await else {
            return;
        };
        let mut tx = db.begin().await.unwrap();
        crate::db::query(&format!("SET LOCAL ROLE {PLUGIN_ROLE_RO}"))
            .execute(&mut *tx)
            .await
            .unwrap();
        let msg = match crate::db::query(
            "SELECT query_to_xml('select * from happyview_users', true, false, '')",
        )
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
}
