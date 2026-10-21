//! The compatibility views that keep `happyview_records` and
//! `happyview_record_refs` readable and writable from raw SQL (`db.raw`,
//! the plugin `host_db_*` imports, Lua) after their data moved to the
//! compact v2 tables.
//!
//! `happyview_records` keeps its columns: `uri` is derived, `record` is the
//! JSON exactly as stored (`hv_record` decompresses it on SQLite; Postgres
//! stores TEXT), and `cid` and the timestamps are rendered back to text.
//! `INSTEAD OF` triggers intern identity and encode values on the way in.
//! What does not carry over, on purpose: an upsert (`INSERT … ON CONFLICT`)
//! on either view fails, and on SQLite writes through a view report no
//! affected rows (SQLite does not count changes made by `INSTEAD OF`
//! triggers) and `INSERT … RETURNING` returns one row of the values the
//! statement supplied, not the stored row (`uri` is NULL when omitted).
//!
//! [`create_statements`] takes the view names so tests can build the views
//! under staging names beside the legacy tables; the upgrade builds them
//! once under [`LIVE`].

use crate::db::DatabaseBackend;

#[derive(Clone, Copy, Debug)]
pub struct CompatNames<'a> {
    pub records: &'a str,
    pub refs: &'a str,
}

pub const LIVE: CompatNames<'static> = CompatNames {
    records: "happyview_records",
    refs: "happyview_record_refs",
};

const URI_MISMATCH: &str = "happyview_records: uri does not match did, collection and rkey";
const NO_SOURCE: &str = "happyview_record_refs: source_uri is not a stored record";
const COLLECTION_MISMATCH: &str =
    "happyview_record_refs: collection does not match the source record";

pub fn create_statements(backend: DatabaseBackend, names: CompatNames<'_>) -> Vec<String> {
    match backend {
        DatabaseBackend::Sqlite => sqlite_statements(names),
        DatabaseBackend::Postgres => postgres_statements(names),
    }
}

const SQLITE_RECORD_EXPR: &str = "hv_record(v.body, v.body_dict)";

/// The two read-only compatibility views, records first. A record provider
/// creates these on its raw read route (see `RecordStorage::raw_route`) with
/// a search path that resolves `happyview_records_v2` and friends to the
/// rows a tenant may see.
pub fn view_statements(backend: DatabaseBackend, names: CompatNames<'_>) -> Vec<String> {
    let record_expr = match backend {
        DatabaseBackend::Sqlite => SQLITE_RECORD_EXPR,
        DatabaseBackend::Postgres => "v.record",
    };
    vec![
        records_view(names.records, record_expr),
        refs_view(names.refs),
    ]
}

pub fn drop_statements(backend: DatabaseBackend, names: CompatNames<'_>) -> Vec<String> {
    let CompatNames {
        records: r,
        refs: f,
    } = names;
    match backend {
        DatabaseBackend::Sqlite => vec![
            format!("DROP VIEW IF EXISTS {f}"),
            format!("DROP VIEW IF EXISTS {r}"),
        ],
        DatabaseBackend::Postgres => vec![
            format!("DROP VIEW IF EXISTS {f}"),
            format!("DROP VIEW IF EXISTS {r}"),
            format!("DROP FUNCTION IF EXISTS {f}_write()"),
            format!("DROP FUNCTION IF EXISTS {r}_write()"),
        ],
    }
}

fn records_view(records: &str, record_expr: &str) -> String {
    format!(
        "CREATE VIEW {records} AS \
         SELECT 'at://' || d.did || '/' || c.nsid || '/' || v.rkey AS uri, \
                d.did AS did, c.nsid AS collection, v.rkey AS rkey, \
                {record_expr} AS record, hv_cid_text(v.cid) AS cid, \
                hv_ts_text(v.indexed_at) AS indexed_at, hv_ts_text(v.created_at) AS created_at, \
                v.rev AS rev \
           FROM happyview_records_v2 v \
           JOIN happyview_dids d ON d.id = v.did_id \
           JOIN happyview_collections c ON c.id = v.collection_id"
    )
}

/// `source_uri` comes from the source row's own collection and `collection`
/// from the ref's `collection_id`. The triggers keep the two equal for new
/// writes, but a legacy ref copied by the upgrade keeps the collection its
/// row held, even one that disagreed with its source.
fn refs_view(refs: &str) -> String {
    format!(
        "CREATE VIEW {refs} AS \
         SELECT 'at://' || sd.did || '/' || ssc.nsid || '/' || s.rkey AS source_uri, \
                CASE f.target_collection_id \
                    WHEN -1 THEN td.did \
                    WHEN 0 THEN 'at://' || td.did \
                    ELSE 'at://' || td.did || '/' || tc.nsid || '/' || f.target_rkey \
                END AS target_uri, \
                sc.nsid AS collection \
           FROM happyview_record_refs_v2 f \
           JOIN happyview_records_v2 s ON s.id = f.source_id \
           JOIN happyview_dids sd ON sd.id = s.did_id \
           JOIN happyview_collections ssc ON ssc.id = s.collection_id \
           JOIN happyview_collections sc ON sc.id = f.collection_id \
           JOIN happyview_dids td ON td.id = f.target_did_id \
           LEFT JOIN happyview_collections tc ON tc.id = f.target_collection_id"
    )
}

/// The v2 id of the record with this identity (SQL expressions), or NULL.
fn record_id(did: &str, collection: &str, rkey: &str) -> String {
    format!(
        "(SELECT v.id FROM happyview_records_v2 v \
          WHERE v.did_id = (SELECT id FROM happyview_dids WHERE did = {did}) \
            AND v.collection_id = (SELECT id FROM happyview_collections WHERE nsid = {collection}) \
            AND v.rkey = {rkey})"
    )
}

fn source_id(uri: &str) -> String {
    record_id(
        &format!("hv_ref_part({uri}, 0)"),
        &format!("hv_ref_part({uri}, 1)"),
        &format!("hv_ref_part({uri}, 2)"),
    )
}

fn target_collection_id(uri: &str) -> String {
    format!(
        "CASE hv_ref_part({uri}, 3) \
            WHEN '1' THEN (SELECT id FROM happyview_collections WHERE nsid = hv_ref_part({uri}, 1)) \
            ELSE CAST(hv_ref_part({uri}, 3) AS INTEGER) END"
    )
}

/// SQLite statements that intern a name without an `ON CONFLICT` an outer
/// `INSERT OR REPLACE` could override.
fn intern_sqlite(table: &str, column: &str, value: &str) -> String {
    format!(
        "INSERT INTO {table} ({column}) SELECT {value} \
         WHERE NOT EXISTS (SELECT 1 FROM {table} WHERE {column} = {value});"
    )
}

fn sqlite_ref_insert(row: &str) -> String {
    let target = format!("{row}.target_uri");
    let source = format!("{row}.source_uri");
    format!(
        "SELECT RAISE(ABORT, '{NO_SOURCE}') \
          WHERE hv_ref_part({source}, 3) IS NOT '1' OR {sid} IS NULL; \
         SELECT RAISE(ABORT, '{COLLECTION_MISMATCH}') \
          WHERE {row}.collection IS NOT hv_ref_part({source}, 1); \
         {did} \
         INSERT INTO happyview_collections (nsid) SELECT hv_ref_part({target}, 1) \
          WHERE hv_ref_part({target}, 1) IS NOT NULL \
            AND NOT EXISTS (SELECT 1 FROM happyview_collections WHERE nsid = hv_ref_part({target}, 1)); \
         INSERT INTO happyview_record_refs_v2 (source_id, target_did_id, target_collection_id, target_rkey, collection_id) \
         VALUES ({sid}, (SELECT id FROM happyview_dids WHERE did = hv_ref_part({target}, 0)), {tcid}, \
                 hv_ref_part({target}, 2), \
                 (SELECT id FROM happyview_collections WHERE nsid = hv_ref_part({source}, 1)));",
        sid = source_id(&source),
        did = intern_sqlite(
            "happyview_dids",
            "did",
            &format!("hv_ref_part({target}, 0)")
        ),
        tcid = target_collection_id(&target),
    )
}

fn sqlite_ref_delete(row: &str) -> String {
    let target = format!("{row}.target_uri");
    format!(
        "DELETE FROM happyview_record_refs_v2 \
          WHERE source_id = {sid} \
            AND target_did_id = (SELECT id FROM happyview_dids WHERE did = hv_ref_part({target}, 0)) \
            AND target_collection_id = {tcid} \
            AND target_rkey = hv_ref_part({target}, 2);",
        sid = source_id(&format!("{row}.source_uri")),
        tcid = target_collection_id(&target),
    )
}

fn sqlite_statements(names: CompatNames<'_>) -> Vec<String> {
    let CompatNames {
        records: r,
        refs: f,
    } = names;
    let old_id = record_id("OLD.did", "OLD.collection", "OLD.rkey");
    let derived = "('at://' || NEW.did || '/' || NEW.collection || '/' || NEW.rkey)";
    let mut statements = view_statements(DatabaseBackend::Sqlite, names);
    statements.extend([
        format!(
            "CREATE TRIGGER {r}_insert INSTEAD OF INSERT ON {r} BEGIN \
             SELECT RAISE(ABORT, '{URI_MISMATCH}') WHERE NEW.uri IS NOT NULL AND NEW.uri IS NOT {derived}; \
             {did} {collection} \
             INSERT INTO happyview_records_v2 (did_id, collection_id, rkey, cid, body, body_dict, rev, indexed_at, created_at) \
             VALUES ((SELECT id FROM happyview_dids WHERE did = NEW.did), \
                     (SELECT id FROM happyview_collections WHERE nsid = NEW.collection), \
                     NEW.rkey, hv_cid_bytes(NEW.cid), CAST(NEW.record AS BLOB), NULL, NEW.rev, \
                     hv_ts_micros(NEW.indexed_at), COALESCE(hv_ts_micros(NEW.created_at), hv_now_micros())); \
             END",
            did = intern_sqlite("happyview_dids", "did", "NEW.did"),
            collection = intern_sqlite("happyview_collections", "nsid", "NEW.collection"),
        ),
        format!(
            "CREATE TRIGGER {r}_update INSTEAD OF UPDATE ON {r} BEGIN \
             SELECT RAISE(ABORT, '{URI_MISMATCH}') WHERE NEW.uri IS NOT OLD.uri AND NEW.uri IS NOT {derived}; \
             {did} {collection} \
             UPDATE happyview_records_v2 SET \
                 did_id = (SELECT id FROM happyview_dids WHERE did = NEW.did), \
                 collection_id = (SELECT id FROM happyview_collections WHERE nsid = NEW.collection), \
                 rkey = NEW.rkey, \
                 cid = hv_cid_bytes(NEW.cid), \
                 body = CASE WHEN NEW.record IS OLD.record THEN body ELSE CAST(NEW.record AS BLOB) END, \
                 body_dict = CASE WHEN NEW.record IS OLD.record THEN body_dict ELSE NULL END, \
                 rev = NEW.rev, \
                 indexed_at = hv_ts_micros(NEW.indexed_at), \
                 created_at = COALESCE(hv_ts_micros(NEW.created_at), created_at) \
             WHERE id = {old_id}; \
             END",
            did = intern_sqlite("happyview_dids", "did", "NEW.did"),
            collection = intern_sqlite("happyview_collections", "nsid", "NEW.collection"),
        ),
        format!(
            "CREATE TRIGGER {r}_delete INSTEAD OF DELETE ON {r} BEGIN \
             DELETE FROM happyview_record_refs_v2 WHERE source_id = {old_id}; \
             DELETE FROM happyview_records_v2 WHERE id = {old_id}; \
             END"
        ),
        format!(
            "CREATE TRIGGER {f}_insert INSTEAD OF INSERT ON {f} BEGIN {} END",
            sqlite_ref_insert("NEW")
        ),
        format!(
            "CREATE TRIGGER {f}_update INSTEAD OF UPDATE ON {f} BEGIN {} {} END",
            sqlite_ref_delete("OLD"),
            sqlite_ref_insert("NEW")
        ),
        format!(
            "CREATE TRIGGER {f}_delete INSTEAD OF DELETE ON {f} BEGIN {} END",
            sqlite_ref_delete("OLD")
        ),
    ]);
    statements
}

fn postgres_statements(names: CompatNames<'_>) -> Vec<String> {
    let CompatNames {
        records: r,
        refs: f,
    } = names;
    let mut statements = view_statements(DatabaseBackend::Postgres, names);
    statements.extend([
        format!(
            r#"CREATE OR REPLACE FUNCTION {r}_write() RETURNS trigger LANGUAGE plpgsql AS $fn$
DECLARE
    v_old bigint;
    v_did bigint;
    v_collection bigint;
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        SELECT v.id INTO v_old FROM happyview_records_v2 v
          JOIN happyview_dids d ON d.id = v.did_id
          JOIN happyview_collections c ON c.id = v.collection_id
         WHERE d.did = OLD.did AND c.nsid = OLD.collection AND v.rkey = OLD.rkey;
    END IF;
    IF TG_OP = 'DELETE' THEN
        DELETE FROM happyview_records_v2 WHERE id = v_old;
        RETURN OLD;
    END IF;
    IF NEW.uri IS NOT NULL
       AND NEW.uri IS DISTINCT FROM 'at://' || NEW.did || '/' || NEW.collection || '/' || NEW.rkey
       AND (TG_OP = 'INSERT' OR NEW.uri IS DISTINCT FROM OLD.uri) THEN
        RAISE EXCEPTION '{URI_MISMATCH}' USING ERRCODE = 'check_violation';
    END IF;
    INSERT INTO happyview_dids (did) VALUES (NEW.did) ON CONFLICT (did) DO NOTHING;
    SELECT id INTO v_did FROM happyview_dids WHERE did = NEW.did;
    INSERT INTO happyview_collections (nsid) VALUES (NEW.collection) ON CONFLICT (nsid) DO NOTHING;
    SELECT id INTO v_collection FROM happyview_collections WHERE nsid = NEW.collection;
    IF TG_OP = 'INSERT' THEN
        INSERT INTO happyview_records_v2 (did_id, collection_id, rkey, cid, record, rev, indexed_at, created_at)
        VALUES (v_did, v_collection, NEW.rkey, hv_cid_bytes(NEW.cid), NEW.record, NEW.rev,
                hv_ts_micros(NEW.indexed_at), COALESCE(hv_ts_micros(NEW.created_at), hv_now_micros()));
    ELSE
        UPDATE happyview_records_v2 SET did_id = v_did, collection_id = v_collection, rkey = NEW.rkey,
               cid = hv_cid_bytes(NEW.cid), record = NEW.record, rev = NEW.rev,
               indexed_at = hv_ts_micros(NEW.indexed_at),
               created_at = COALESCE(hv_ts_micros(NEW.created_at), created_at)
         WHERE id = v_old;
    END IF;
    NEW.uri := 'at://' || NEW.did || '/' || NEW.collection || '/' || NEW.rkey;
    RETURN NEW;
END $fn$"#
        ),
        format!(
            "CREATE TRIGGER {r}_write INSTEAD OF INSERT OR UPDATE OR DELETE ON {r} FOR EACH ROW EXECUTE FUNCTION {r}_write()"
        ),
        format!(
            r#"CREATE OR REPLACE FUNCTION {f}_write() RETURNS trigger LANGUAGE plpgsql AS $fn$
DECLARE
    v_source bigint;
    v_source_collection bigint;
    v_target_did bigint;
    v_target_collection bigint;
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        SELECT v.id INTO v_source FROM happyview_records_v2 v
          JOIN happyview_dids d ON d.id = v.did_id
          JOIN happyview_collections c ON c.id = v.collection_id
         WHERE d.did = hv_ref_part(OLD.source_uri, 0) AND c.nsid = hv_ref_part(OLD.source_uri, 1)
           AND v.rkey = hv_ref_part(OLD.source_uri, 2);
        SELECT id INTO v_target_did FROM happyview_dids WHERE did = hv_ref_part(OLD.target_uri, 0);
        IF hv_ref_part(OLD.target_uri, 3) = '1' THEN
            SELECT id INTO v_target_collection FROM happyview_collections WHERE nsid = hv_ref_part(OLD.target_uri, 1);
        ELSE
            v_target_collection := hv_ref_part(OLD.target_uri, 3)::bigint;
        END IF;
        DELETE FROM happyview_record_refs_v2
         WHERE source_id = v_source AND target_did_id = v_target_did
           AND target_collection_id = v_target_collection
           AND target_rkey = hv_ref_part(OLD.target_uri, 2);
        IF TG_OP = 'DELETE' THEN
            RETURN OLD;
        END IF;
    END IF;
    v_source := NULL;
    IF hv_ref_part(NEW.source_uri, 3) = '1' THEN
        SELECT v.id, v.collection_id INTO v_source, v_source_collection FROM happyview_records_v2 v
          JOIN happyview_dids d ON d.id = v.did_id
          JOIN happyview_collections c ON c.id = v.collection_id
         WHERE d.did = hv_ref_part(NEW.source_uri, 0) AND c.nsid = hv_ref_part(NEW.source_uri, 1)
           AND v.rkey = hv_ref_part(NEW.source_uri, 2);
    END IF;
    IF v_source IS NULL THEN
        RAISE EXCEPTION '{NO_SOURCE}' USING ERRCODE = 'foreign_key_violation';
    END IF;
    IF NEW.collection IS DISTINCT FROM hv_ref_part(NEW.source_uri, 1) THEN
        RAISE EXCEPTION '{COLLECTION_MISMATCH}' USING ERRCODE = 'check_violation';
    END IF;
    INSERT INTO happyview_dids (did) VALUES (hv_ref_part(NEW.target_uri, 0)) ON CONFLICT (did) DO NOTHING;
    SELECT id INTO v_target_did FROM happyview_dids WHERE did = hv_ref_part(NEW.target_uri, 0);
    IF hv_ref_part(NEW.target_uri, 3) = '1' THEN
        INSERT INTO happyview_collections (nsid) VALUES (hv_ref_part(NEW.target_uri, 1)) ON CONFLICT (nsid) DO NOTHING;
        SELECT id INTO v_target_collection FROM happyview_collections WHERE nsid = hv_ref_part(NEW.target_uri, 1);
    ELSE
        v_target_collection := hv_ref_part(NEW.target_uri, 3)::bigint;
    END IF;
    INSERT INTO happyview_record_refs_v2 (source_id, target_did_id, target_collection_id, target_rkey, collection_id)
    VALUES (v_source, v_target_did, v_target_collection, hv_ref_part(NEW.target_uri, 2), v_source_collection);
    RETURN NEW;
END $fn$"#
        ),
        format!(
            "CREATE TRIGGER {f}_write INSTEAD OF INSERT OR UPDATE OR DELETE ON {f} FOR EACH ROW EXECUTE FUNCTION {f}_write()"
        ),
    ]);
    statements
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::pre_upgrade_pools;

    #[test]
    fn view_statements_are_the_two_read_views() {
        for backend in [DatabaseBackend::Sqlite, DatabaseBackend::Postgres] {
            let s = view_statements(backend, LIVE);
            assert_eq!(s.len(), 2, "{backend:?}");
            assert!(
                s[0].starts_with("CREATE VIEW happyview_records AS"),
                "{}",
                s[0]
            );
            assert!(
                s[1].starts_with("CREATE VIEW happyview_record_refs AS"),
                "{}",
                s[1]
            );
            let all = create_statements(backend, LIVE);
            assert_eq!(
                &all[..2],
                &s[..],
                "create_statements must start with the read views"
            );
        }
    }

    struct Staged {
        records: String,
        refs: String,
    }

    impl Staged {
        fn new() -> Self {
            let tag = &uuid::Uuid::new_v4().simple().to_string()[..8];
            Self {
                records: format!("hvt_records_{tag}"),
                refs: format!("hvt_refs_{tag}"),
            }
        }

        fn names(&self) -> CompatNames<'_> {
            CompatNames {
                records: &self.records,
                refs: &self.refs,
            }
        }
    }

    async fn create(pool: &sqlx::AnyPool, backend: DatabaseBackend, staged: &Staged) {
        for sql in create_statements(backend, staged.names()) {
            crate::db::query(&sql)
                .execute(pool)
                .await
                .unwrap_or_else(|e| panic!("{backend:?}: {e}\n{sql}"));
        }
    }

    /// Raw SQL, written in each backend's own dialect as a plugin would.
    fn ph(backend: DatabaseBackend, n: usize) -> String {
        match backend {
            DatabaseBackend::Sqlite => "?".to_string(),
            DatabaseBackend::Postgres => format!("${n}"),
        }
    }

    async fn insert(
        pool: &sqlx::AnyPool,
        backend: DatabaseBackend,
        view: &str,
        did: &str,
        rkey: &str,
        record: &str,
    ) -> Result<u64, sqlx::Error> {
        let sql = format!(
            "INSERT INTO {view} (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
             VALUES ({}, {}, 'app.test.compat', {}, {}, 'bafyreie5737gdxlw5i64vzichcalba3z2v5n6icifvx5xytvske7mr3hpm', '2026-01-01T00:00:00Z', '2026-01-02T03:04:05.123456Z')",
            ph(backend, 1),
            ph(backend, 2),
            ph(backend, 3),
            ph(backend, 4)
        );
        crate::db::query(&sql)
            .bind(format!("at://{did}/app.test.compat/{rkey}"))
            .bind(did)
            .bind(rkey)
            .bind(record)
            .execute(pool)
            .await
            .map(|r| r.rows_affected())
    }

    const BODY: &str = r#"{"text":"hi","via":"witchsky","z":[1,{"é":"\n"}],"a":true}"#;

    #[tokio::test]
    async fn records_written_through_the_view_read_back_exactly() {
        for db in pre_upgrade_pools().await {
            let (pool, backend, staged) = (&db.pool, db.backend, Staged::new());
            create(pool, backend, &staged).await;
            insert(pool, backend, &staged.records, "did:plc:compat", "a", BODY)
                .await
                .expect("insert");
            let (uri, record, cid, indexed_at, created_at): (String, String, String, Option<String>, String) =
                crate::db::query_as(&format!(
                    "SELECT uri, record, cid, indexed_at, created_at FROM {} WHERE did = 'did:plc:compat'",
                    staged.records
                ))
                .fetch_one(pool)
                .await
                .expect("read back");
            assert_eq!(uri, "at://did:plc:compat/app.test.compat/a");
            assert_eq!(record, BODY, "{backend:?}: byte-for-byte");
            assert_eq!(
                cid,
                "bafyreie5737gdxlw5i64vzichcalba3z2v5n6icifvx5xytvske7mr3hpm"
            );
            assert_eq!(
                indexed_at.as_deref(),
                Some("2026-01-01T00:00:00.000000+00:00")
            );
            assert_eq!(created_at, "2026-01-02T03:04:05.123456+00:00");

            let filters: &[&str] = match backend {
                DatabaseBackend::Sqlite => &["json_extract(record, '$.via')", "record ->> '$.via'"],
                DatabaseBackend::Postgres => &["record::jsonb->>'via'"],
            };
            for filter in filters {
                let rows: Vec<(String, String)> = crate::db::query_as(&format!(
                    "SELECT uri, {filter} FROM {} WHERE {filter} = 'witchsky'",
                    staged.records
                ))
                .fetch_all(pool)
                .await
                .unwrap_or_else(|e| panic!("{backend:?} {filter}: {e}"));
                assert_eq!(
                    rows,
                    vec![(uri.clone(), "witchsky".to_string())],
                    "{backend:?} {filter}"
                );
            }
            db.finish().await;
        }
    }

    #[tokio::test]
    async fn updates_and_deletes_through_the_view_reach_the_v2_tables() {
        for db in pre_upgrade_pools().await {
            let (pool, backend, staged) = (&db.pool, db.backend, Staged::new());
            create(pool, backend, &staged).await;
            insert(pool, backend, &staged.records, "did:plc:upd", "a", BODY)
                .await
                .expect("insert");
            let uri = "at://did:plc:upd/app.test.compat/a";

            crate::db::query(&format!(
                "UPDATE {} SET indexed_at = NULL WHERE did = 'did:plc:upd' AND rkey = 'a'",
                staged.records
            ))
            .execute(pool)
            .await
            .expect("clear indexed_at");
            let (record, indexed_at): (String, Option<String>) = crate::db::query_as(&format!(
                "SELECT record, indexed_at FROM {} WHERE did = 'did:plc:upd'",
                staged.records
            ))
            .fetch_one(pool)
            .await
            .expect("read");
            assert_eq!(
                (record.as_str(), indexed_at),
                (BODY, None),
                "{backend:?}: only indexed_at changed"
            );

            crate::db::query(&format!(
                "UPDATE {} SET record = '{{\"n\":2}}' WHERE uri = '{uri}'",
                staged.records
            ))
            .execute(pool)
            .await
            .expect("update record");
            let (record,): (String,) = crate::db::query_as(&format!(
                "SELECT record FROM {} WHERE did = 'did:plc:upd'",
                staged.records
            ))
            .fetch_one(pool)
            .await
            .expect("read");
            assert_eq!(record, r#"{"n":2}"#);

            crate::db::query(&format!(
                "INSERT INTO {} (source_uri, target_uri, collection) VALUES ('{uri}', 'at://did:plc:x/app.test.compat/b', 'app.test.compat')",
                staged.refs
            ))
            .execute(pool)
            .await
            .expect("insert a ref");
            crate::db::query(&format!(
                "DELETE FROM {} WHERE uri = '{uri}'",
                staged.records
            ))
            .execute(pool)
            .await
            .expect("delete");
            for (table, expected) in [("happyview_records_v2", 0), ("happyview_record_refs_v2", 0)]
            {
                let (n,): (i64,) = crate::db::query_as(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(pool)
                    .await
                    .expect("count");
                assert_eq!(n, expected, "{backend:?} {table}");
            }
            db.finish().await;
        }
    }

    #[tokio::test]
    async fn a_uri_that_disagrees_with_its_identity_is_refused() {
        for db in pre_upgrade_pools().await {
            let (pool, backend, staged) = (&db.pool, db.backend, Staged::new());
            create(pool, backend, &staged).await;
            let err = crate::db::query(&format!(
                "INSERT INTO {} (uri, did, collection, rkey, record, cid, created_at) \
                 VALUES ('at://a/c/1', 'did:plc:a', 'c', '1', '{{}}', 'c1', '2026-01-01T00:00:00Z')",
                staged.records
            ))
            .execute(pool)
            .await
            .expect_err("a uri naming another identity");
            assert!(
                err.to_string().contains("uri does not match"),
                "{backend:?}: {err}"
            );
            db.finish().await;
        }
    }

    /// Review Focus 2: an upsert through the view errors on both backends
    /// rather than silently doing nothing; plain writes, and SQLite's
    /// INSERT OR REPLACE, still work.
    #[tokio::test]
    async fn raw_upserts_through_the_view_fail_loudly() {
        for db in pre_upgrade_pools().await {
            let (pool, backend, staged) = (&db.pool, db.backend, Staged::new());
            create(pool, backend, &staged).await;
            insert(pool, backend, &staged.records, "did:plc:ups", "a", BODY)
                .await
                .expect("insert");
            let columns = "(uri, did, collection, rkey, record, cid, created_at)";
            let values = "('at://did:plc:ups/app.test.compat/a', 'did:plc:ups', 'app.test.compat', 'a', '{\"n\":9}', 'c', '2026-01-01T00:00:00Z')";
            for clause in [
                "ON CONFLICT (uri) DO UPDATE SET record = excluded.record",
                "ON CONFLICT DO NOTHING",
                "",
            ] {
                let sql = format!(
                    "INSERT INTO {} {columns} VALUES {values} {clause}",
                    staged.records
                );
                assert!(
                    crate::db::query(&sql).execute(pool).await.is_err(),
                    "{backend:?}: {clause:?} on an existing row must fail"
                );
            }
            if backend == DatabaseBackend::Sqlite {
                crate::db::query(&format!(
                    "INSERT OR REPLACE INTO {} {columns} VALUES {values}",
                    staged.records
                ))
                .execute(pool)
                .await
                .expect("INSERT OR REPLACE replaces the row");
                let (record,): (String,) = crate::db::query_as(&format!(
                    "SELECT record FROM {} WHERE did = 'did:plc:ups'",
                    staged.records
                ))
                .fetch_one(pool)
                .await
                .expect("read");
                assert_eq!(record, r#"{"n":9}"#);
                let (dids,): (i64,) = crate::db::query_as(
                    "SELECT COUNT(*) FROM happyview_dids WHERE did = 'did:plc:ups'",
                )
                .fetch_one(pool)
                .await
                .expect("count");
                assert_eq!(dids, 1, "the DID row was not replaced");
            }
            db.finish().await;
        }
    }

    /// SQLite's `INSERT OR REPLACE` through the view replaces the v2 row,
    /// and its refs go with the old row, as they did on the legacy table:
    /// nothing is orphaned and no foreign key is left dangling.
    #[tokio::test]
    async fn sqlite_insert_or_replace_drops_the_old_refs_cleanly() {
        for db in pre_upgrade_pools().await {
            let (pool, backend, staged) = (&db.pool, db.backend, Staged::new());
            if backend != DatabaseBackend::Sqlite {
                db.finish().await;
                continue;
            }
            create(pool, backend, &staged).await;
            insert(pool, backend, &staged.records, "did:plc:rep", "a", BODY)
                .await
                .expect("insert");
            insert(pool, backend, &staged.records, "did:plc:rep", "b", BODY)
                .await
                .expect("insert a neighbour");
            let (a, b) = (
                "at://did:plc:rep/app.test.compat/a",
                "at://did:plc:rep/app.test.compat/b",
            );
            for (source, target) in [
                (a, "at://did:plc:x/app.test.compat/1"),
                (a, "at://did:plc:x/app.test.compat/2"),
                (b, "at://did:plc:x/app.test.compat/3"),
            ] {
                crate::db::query(&format!(
                    "INSERT INTO {} (source_uri, target_uri, collection) VALUES ('{source}', '{target}', 'app.test.compat')",
                    staged.refs
                ))
                .execute(pool)
                .await
                .expect("insert a ref");
            }

            crate::db::query(&format!(
                "INSERT OR REPLACE INTO {} (uri, did, collection, rkey, record, cid, created_at) \
                 VALUES ('{a}', 'did:plc:rep', 'app.test.compat', 'a', '{{\"n\":9}}', 'c', '2026-01-01T00:00:00Z')",
                staged.records
            ))
            .execute(pool)
            .await
            .expect("INSERT OR REPLACE");

            let (record,): (String,) = crate::db::query_as(&format!(
                "SELECT record FROM {} WHERE uri = '{a}'",
                staged.records
            ))
            .fetch_one(pool)
            .await
            .expect("read the replaced row");
            assert_eq!(record, r#"{"n":9}"#);
            let refs: Vec<(String, String)> = crate::db::query_as(&format!(
                "SELECT source_uri, target_uri FROM {} ORDER BY target_uri",
                staged.refs
            ))
            .fetch_all(pool)
            .await
            .expect("read refs");
            assert_eq!(
                refs,
                vec![(
                    b.to_string(),
                    "at://did:plc:x/app.test.compat/3".to_string()
                )],
                "the replaced row's refs are gone, its neighbour's kept"
            );
            let (orphans,): (i64,) = crate::db::query_as(
                "SELECT COUNT(*) FROM happyview_record_refs_v2 f \
                 WHERE NOT EXISTS (SELECT 1 FROM happyview_records_v2 v WHERE v.id = f.source_id)",
            )
            .fetch_one(pool)
            .await
            .expect("count orphans");
            assert_eq!(orphans, 0);
            let violations = crate::db::query("PRAGMA foreign_key_check")
                .fetch_all(pool)
                .await
                .expect("foreign_key_check");
            assert!(
                violations.is_empty(),
                "{} foreign key violations",
                violations.len()
            );
            db.finish().await;
        }
    }

    /// Review Focus 5.
    #[tokio::test]
    async fn odd_ref_targets_round_trip_through_the_refs_view() {
        let targets = [
            "at://did:plc:t/app.test.compat/r",
            "at://did:plc:t",
            "at://did:plc:t/",
            "at://alice.example.com/app.test.compat/r/extra",
            "https://example.com/x",
        ];
        for db in pre_upgrade_pools().await {
            let (pool, backend, staged) = (&db.pool, db.backend, Staged::new());
            create(pool, backend, &staged).await;
            insert(pool, backend, &staged.records, "did:plc:src", "s", BODY)
                .await
                .expect("source");
            let source = "at://did:plc:src/app.test.compat/s";
            for target in targets {
                crate::db::query(&format!(
                    "INSERT INTO {} (source_uri, target_uri, collection) VALUES ('{source}', '{target}', 'app.test.compat')",
                    staged.refs
                ))
                .execute(pool)
                .await
                .unwrap_or_else(|e| panic!("{backend:?} {target}: {e}"));
            }
            let mut read: Vec<String> = crate::db::query_as::<(String, String, String)>(&format!(
                "SELECT source_uri, target_uri, collection FROM {}",
                staged.refs
            ))
            .fetch_all(pool)
            .await
            .expect("read refs")
            .into_iter()
            .map(|(s, t, c)| {
                assert_eq!((s.as_str(), c.as_str()), (source, "app.test.compat"));
                t
            })
            .collect();
            read.sort();
            let mut expected: Vec<String> = targets.iter().map(|t| t.to_string()).collect();
            expected.sort();
            assert_eq!(read, expected, "{backend:?}");

            crate::db::query(&format!(
                "DELETE FROM {} WHERE target_uri = 'at://did:plc:t'",
                staged.refs
            ))
            .execute(pool)
            .await
            .expect("delete one ref");
            let (left,): (i64,) =
                crate::db::query_as(&format!("SELECT COUNT(*) FROM {}", staged.refs))
                    .fetch_one(pool)
                    .await
                    .expect("count");
            assert_eq!(left, targets.len() as i64 - 1, "{backend:?}");

            for (source, collection) in [
                ("at://did:plc:nobody/app.test.compat/s", "app.test.compat"),
                (source, "app.test.other"),
            ] {
                assert!(
                    crate::db::query(&format!(
                        "INSERT INTO {} (source_uri, target_uri, collection) VALUES ('{source}', 'at://x/y/z', '{collection}')",
                        staged.refs
                    ))
                    .execute(pool)
                    .await
                    .is_err(),
                    "{backend:?}: {source} {collection}"
                );
            }
            db.finish().await;
        }
    }

    /// Ruling T10-1: an omitted `indexed_at` is NULL (a local write awaiting
    /// its echo) and an omitted `created_at` is now. On SQLite the body is
    /// stored raw for the background task to compress, and an update that
    /// leaves `record` alone keeps the stored encoding.
    #[tokio::test]
    async fn omitted_timestamps_and_sqlite_body_encoding() {
        for db in pre_upgrade_pools().await {
            let (pool, backend, staged) = (&db.pool, db.backend, Staged::new());
            create(pool, backend, &staged).await;
            crate::db::query(&format!(
                "INSERT INTO {} (did, collection, rkey, record, cid) \
                 VALUES ('did:plc:omit', 'app.test.compat', 'a', '{{}}', 'c1')",
                staged.records
            ))
            .execute(pool)
            .await
            .expect("insert without uri or timestamps");
            let (uri, indexed_at, created_at): (String, Option<String>, String) =
                crate::db::query_as(&format!(
                    "SELECT uri, indexed_at, created_at FROM {} WHERE did = 'did:plc:omit'",
                    staged.records
                ))
                .fetch_one(pool)
                .await
                .expect("read");
            assert_eq!(uri, "at://did:plc:omit/app.test.compat/a", "{backend:?}");
            assert_eq!(indexed_at, None, "{backend:?}");
            assert!(created_at.starts_with("20"), "{backend:?}: {created_at}");

            if backend == DatabaseBackend::Sqlite {
                let (dict,): (Option<i64>,) =
                    crate::db::query_as("SELECT body_dict FROM happyview_records_v2")
                        .fetch_one(pool)
                        .await
                        .expect("read body_dict");
                assert_eq!(dict, None, "a trigger write is not yet considered");
                crate::db::query("UPDATE happyview_records_v2 SET body_dict = 0")
                    .execute(pool)
                    .await
                    .expect("mark considered");
                crate::db::query(&format!(
                    "UPDATE {} SET indexed_at = '2026-01-01T00:00:00Z' WHERE did = 'did:plc:omit'",
                    staged.records
                ))
                .execute(pool)
                .await
                .expect("stamp");
                let (dict,): (Option<i64>,) =
                    crate::db::query_as("SELECT body_dict FROM happyview_records_v2")
                        .fetch_one(pool)
                        .await
                        .expect("read body_dict");
                assert_eq!(dict, Some(0), "an unchanged record keeps its encoding");
                crate::db::query(&format!(
                    "UPDATE {} SET record = '{{\"n\":1}}' WHERE did = 'did:plc:omit'",
                    staged.records
                ))
                .execute(pool)
                .await
                .expect("edit");
                let (dict,): (Option<i64>,) =
                    crate::db::query_as("SELECT body_dict FROM happyview_records_v2")
                        .fetch_one(pool)
                        .await
                        .expect("read body_dict");
                assert_eq!(dict, None, "a new body is reconsidered");
            }
            db.finish().await;
        }
    }

    /// The views keep the legacy tables' columns, in order, so `SELECT *`
    /// and positional `INSERT`s read and write as before.
    #[tokio::test]
    async fn the_views_have_the_legacy_columns_in_order() {
        for db in pre_upgrade_pools().await {
            let (pool, backend, staged) = (&db.pool, db.backend, Staged::new());
            create(pool, backend, &staged).await;
            let columns = |relation: String| {
                let sql = match backend {
                    DatabaseBackend::Sqlite => {
                        format!("SELECT name FROM pragma_table_info('{relation}') ORDER BY cid")
                    }
                    DatabaseBackend::Postgres => format!(
                        "SELECT column_name::text FROM information_schema.columns \
                         WHERE table_schema = current_schema() AND table_name = '{relation}' \
                         ORDER BY ordinal_position"
                    ),
                };
                async move {
                    crate::db::query_as::<(String,)>(&sql)
                        .fetch_all(pool)
                        .await
                        .expect("list columns")
                        .into_iter()
                        .map(|(c,)| c)
                        .collect::<Vec<_>>()
                }
            };
            for (legacy, view) in [
                (LIVE.records, staged.records.as_str()),
                (LIVE.refs, staged.refs.as_str()),
            ] {
                let expected = columns(legacy.to_string()).await;
                assert!(!expected.is_empty(), "{backend:?}: {legacy} exists");
                assert_eq!(
                    columns(view.to_string()).await,
                    expected,
                    "{backend:?} {legacy}"
                );
            }
            db.finish().await;
        }
    }

    #[tokio::test]
    async fn the_views_can_be_dropped_and_recreated() {
        for db in pre_upgrade_pools().await {
            let (pool, backend, staged) = (&db.pool, db.backend, Staged::new());
            create(pool, backend, &staged).await;
            for sql in drop_statements(backend, staged.names()) {
                crate::db::query(&sql)
                    .execute(pool)
                    .await
                    .unwrap_or_else(|e| panic!("{e}\n{sql}"));
            }
            create(pool, backend, &staged).await;
            db.finish().await;
        }
    }
}
