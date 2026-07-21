use std::collections::{BTreeSet, HashMap};

use crate::db::{DatabaseBackend, adapt_sql};
use crate::error::AppError;
use crate::lua::tid::generate_tid;
use crate::spaces::lthash::{LtHashState, record_element};
use crate::spaces::{commit, db};

type RecordVersion = (String, String, String, String, String);

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackfillReport {
    pub records_scanned: usize,
    pub records_updated: usize,
    pub records_unencodable: usize,
    pub oplog_rows_remapped: usize,
    pub oplog_rows_unresolved: usize,
    pub repos_rebuilt: usize,
}

impl BackfillReport {
    pub fn is_noop(&self) -> bool {
        self.records_updated == 0 && self.oplog_rows_remapped == 0
    }
}

type RecordRow = (String, String, String, String, String, String, String);

pub async fn run(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    dry_run: bool,
) -> Result<BackfillReport, AppError> {
    let mut report = BackfillReport::default();

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

    // ---------------------------------------------------------------------
    // 1. Recompute record CIDs
    // ---------------------------------------------------------------------
    let sql = adapt_sql(
        "SELECT uri, space_id, author_did, collection, rkey, record, cid FROM happyview_space_records ORDER BY uri",
        backend,
    );
    let rows: Vec<RecordRow> = crate::db::query_as(&sql)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load space records: {e}")))?;

    // (space, author, collection, rkey, old_cid) -> new_cid
    let mut remap: HashMap<RecordVersion, String> = HashMap::new();
    // (space_id, author_did) pairs whose repo state must be rebuilt.
    let mut repos: BTreeSet<(String, String)> = BTreeSet::new();

    for (uri, space_id, author_did, collection, rkey, record_json, old_cid) in rows {
        report.records_scanned += 1;
        repos.insert((space_id.clone(), author_did.clone()));

        let value: serde_json::Value = serde_json::from_str(&record_json)
            .map_err(|e| AppError::Internal(format!("record {uri} has unparseable JSON: {e}")))?;

        let Some(new_cid) = crate::cid_verify::compute_record_cid(&value).map(|c| c.to_string())
        else {
            report.records_unencodable += 1;
            tracing::warn!(
                uri,
                "record cannot be encoded as DAG-CBOR; leaving CID unchanged"
            );
            continue;
        };

        if new_cid == old_cid {
            continue;
        }

        remap.insert(
            (space_id, author_did, collection, rkey, old_cid),
            new_cid.clone(),
        );

        let update = adapt_sql(
            "UPDATE happyview_space_records SET cid = ? WHERE uri = ?",
            backend,
        );
        crate::db::query(&update)
            .bind(&new_cid)
            .bind(&uri)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(format!("failed to update CID for {uri}: {e}")))?;
        report.records_updated += 1;
    }

    // ---------------------------------------------------------------------
    // 2. Remap oplog CIDs
    // ---------------------------------------------------------------------
    let sql = adapt_sql(
        "SELECT id, space_id, author_did, collection, rkey, cid, prev FROM happyview_space_record_oplog",
        backend,
    );
    type OplogRow = (
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    );
    let ops: Vec<OplogRow> = crate::db::query_as(&sql)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load oplog: {e}")))?;

    for (id, space_id, author_did, collection, rkey, cid, prev) in ops {
        let lookup = |c: &str| -> Option<String> {
            remap
                .get(&(
                    space_id.clone(),
                    author_did.clone(),
                    collection.clone(),
                    rkey.clone(),
                    c.to_string(),
                ))
                .cloned()
        };

        let new_cid = cid.as_deref().and_then(&lookup);
        let new_prev = prev.as_deref().and_then(&lookup);

        // An op referencing a CID we could not remap points at content that is
        // gone (superseded or deleted), so it keeps its placeholder value.
        let unresolved = (cid.is_some() && new_cid.is_none() && is_placeholder(cid.as_deref()))
            || (prev.is_some() && new_prev.is_none() && is_placeholder(prev.as_deref()));
        if unresolved {
            report.oplog_rows_unresolved += 1;
        }

        if new_cid.is_none() && new_prev.is_none() {
            continue;
        }

        let update = adapt_sql(
            "UPDATE happyview_space_record_oplog SET cid = ?, prev = ? WHERE id = ?",
            backend,
        );
        crate::db::query(&update)
            .bind(new_cid.or(cid))
            .bind(new_prev.or(prev))
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(format!("failed to remap oplog entry {id}: {e}")))?;
        report.oplog_rows_remapped += 1;
    }

    // ---------------------------------------------------------------------
    // 3. Rebuild repo state
    // ---------------------------------------------------------------------
    // Include repos that already have a commit even if they now hold no records,
    // so an emptied repo is rebuilt to the empty-set hash rather than left stale.
    let sql = adapt_sql(
        "SELECT space_id, author_did FROM happyview_space_repo_state WHERE hash IS NOT NULL",
        backend,
    );
    let existing: Vec<(String, String)> = crate::db::query_as(&sql)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load repo states: {e}")))?;
    repos.extend(existing);

    for (space_id, author_did) in repos {
        let Some(space) = db::get_space(&mut *tx, backend, &space_id).await? else {
            tracing::warn!(space_id, "repo state references a missing space; skipping");
            continue;
        };

        let sql = adapt_sql(
            "SELECT collection, rkey, cid FROM happyview_space_records WHERE space_id = ? AND author_did = ?",
            backend,
        );
        let records: Vec<(String, String, String)> = crate::db::query_as(&sql)
            .bind(&space_id)
            .bind(&author_did)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(format!("failed to load records for rebuild: {e}")))?;

        let mut set_hash = LtHashState::new();
        for (collection, rkey, cid) in &records {
            set_hash.add(&record_element(collection, rkey, cid));
        }

        let space_uri = format!(
            "at://{}/space/{}/{}",
            space.did, space.type_nsid, space.skey
        );
        let rev = generate_tid();
        let signed = commit::sign_commit(&set_hash.hash(), &space_uri, &author_did, &rev)?;

        let mut repo_state =
            db::get_or_create_repo_state(&mut tx, backend, &space_id, &author_did).await?;
        repo_state.lthash_state = set_hash.as_bytes().to_vec();
        repo_state.rev = Some(signed.rev);
        repo_state.hash = Some(signed.hash.to_vec());
        repo_state.ikm = Some(signed.ikm.to_vec());
        repo_state.mac = Some(signed.mac.to_vec());
        db::update_repo_state(&mut *tx, backend, &repo_state).await?;

        db::update_space_revision(&mut *tx, backend, &space_id, &rev).await?;
        report.repos_rebuilt += 1;
    }

    if dry_run {
        tx.rollback()
            .await
            .map_err(|e| AppError::Internal(format!("failed to roll back dry run: {e}")))?;
    } else {
        tx.commit()
            .await
            .map_err(|e| AppError::Internal(format!("failed to commit repair: {e}")))?;
    }

    Ok(report)
}

fn is_placeholder(cid: Option<&str>) -> bool {
    let Some(cid) = cid else { return false };
    let Some(rest) = cid.strip_prefix("bafyrei") else {
        return false;
    };
    rest.len() == 40 && rest.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_legacy_placeholder() {
        let legacy = format!("bafyrei{}", "a".repeat(40));
        assert!(is_placeholder(Some(&legacy)));
    }

    #[test]
    fn real_cid_is_not_a_placeholder() {
        let real = crate::cid_verify::compute_record_cid(&serde_json::json!({ "a": 1 }))
            .expect("encodable")
            .to_string();
        assert!(real.starts_with("bafyrei"), "sanity: {real}");
        assert!(!is_placeholder(Some(&real)));
    }

    #[test]
    fn absent_cid_is_not_a_placeholder() {
        assert!(!is_placeholder(None));
    }

    #[test]
    fn report_noop_detection() {
        let mut report = BackfillReport::default();
        assert!(report.is_noop());
        report.records_updated = 1;
        assert!(!report.is_noop());
    }
}
