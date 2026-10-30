use serde::Serialize;
use uuid::Uuid;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339, query, query_as};
use crate::error::AppError;

use super::Pattern;

#[derive(Debug, Clone, Serialize)]
pub struct Rule {
    pub id: String,
    pub pattern: String,
    pub target_kind: String,
    pub linked_repo_id: Option<String>,
    pub auto_publish: bool,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Rule {
    /// Stored patterns come from `Pattern::as_stored`, so a parse failure means
    /// the row was written by hand; matching it exactly is the narrowest reading.
    pub fn parsed(&self) -> Pattern {
        Pattern::parse(&self.pattern).unwrap_or_else(|_| Pattern::Exact(self.pattern.clone()))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Publication {
    pub nsid: String,
    pub did: Option<String>,
    pub uri: Option<String>,
    pub cid: Option<String>,
    pub published_at: Option<String>,
    pub last_error: Option<String>,
    pub last_attempt_at: Option<String>,
    /// Set by an explicit unpublish, which keeps the lexicon out of
    /// auto-publish until someone publishes it by hand.
    pub withdrawn_at: Option<String>,
}

type RuleRow = (
    String,
    String,
    String,
    Option<String>,
    i32,
    String,
    String,
    String,
);

// The boolean is read through an integer cast because Postgres and SQLite
// disagree on how the Any driver decodes the column.
const RULE_COLUMNS: &str = "id, pattern, target_kind, linked_repo_id, \
    CAST(auto_publish AS INTEGER), created_by, created_at, updated_at";

fn rule_from_row(r: RuleRow) -> Rule {
    Rule {
        id: r.0,
        pattern: r.1,
        target_kind: r.2,
        linked_repo_id: r.3,
        auto_publish: r.4 != 0,
        created_by: r.5,
        created_at: r.6,
        updated_at: r.7,
    }
}

type PublicationRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

const PUBLICATION_COLUMNS: &str =
    "nsid, did, uri, cid, published_at, last_error, last_attempt_at, withdrawn_at";

fn publication_from_row(r: PublicationRow) -> Publication {
    Publication {
        nsid: r.0,
        did: r.1,
        uri: r.2,
        cid: r.3,
        published_at: r.4,
        last_error: r.5,
        last_attempt_at: r.6,
        withdrawn_at: r.7,
    }
}

fn internal(context: &str, e: sqlx::Error) -> AppError {
    AppError::Internal(format!("{context}: {e}"))
}

/// The handlers check the linked repo before writing, so a foreign-key
/// violation means it was removed between that check and this write.
fn rule_write_error(context: &str, e: sqlx::Error) -> AppError {
    if e.as_database_error()
        .is_some_and(|d| d.is_foreign_key_violation())
    {
        AppError::BadRequest("unknown linked repo".into())
    } else {
        internal(context, e)
    }
}

pub async fn list_rules(state: &AppState) -> Result<Vec<Rule>, AppError> {
    let sql = adapt_sql(
        &format!(
            "SELECT {RULE_COLUMNS} FROM happyview_lexicon_publish_rules ORDER BY created_at, id"
        ),
        state.db_backend,
    );
    let rows: Vec<RuleRow> = query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| internal("failed to list lexicon publish rules", e))?;
    Ok(rows.into_iter().map(rule_from_row).collect())
}

pub async fn get_rule(state: &AppState, id: &str) -> Result<Option<Rule>, AppError> {
    let sql = adapt_sql(
        &format!("SELECT {RULE_COLUMNS} FROM happyview_lexicon_publish_rules WHERE id = ?"),
        state.db_backend,
    );
    let row: Option<RuleRow> = query_as(&sql)
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| internal("failed to read lexicon publish rule", e))?;
    Ok(row.map(rule_from_row))
}

pub async fn insert_rule(
    state: &AppState,
    pattern: &Pattern,
    target_kind: &str,
    linked_repo_id: Option<&str>,
    auto_publish: bool,
    created_by: &str,
) -> Result<Rule, AppError> {
    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let stored = pattern.as_stored();

    let sql = adapt_sql(
        "INSERT INTO happyview_lexicon_publish_rules \
         (id, pattern, target_kind, linked_repo_id, auto_publish, created_by, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        state.db_backend,
    );
    query(&sql)
        .bind(&id)
        .bind(&stored)
        .bind(target_kind)
        .bind(linked_repo_id)
        .bind(auto_publish)
        .bind(created_by)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| {
            if e.as_database_error()
                .is_some_and(|d| d.is_unique_violation())
            {
                AppError::Conflict(format!("a rule for {stored} already exists"))
            } else {
                rule_write_error("failed to create lexicon publish rule", e)
            }
        })?;

    get_rule(state, &id)
        .await?
        .ok_or_else(|| AppError::Internal("lexicon publish rule vanished after insert".into()))
}

/// `linked_repo_id` is doubly optional: `None` leaves it alone, `Some(None)`
/// clears it.
pub async fn update_rule(
    state: &AppState,
    id: &str,
    target_kind: Option<&str>,
    linked_repo_id: Option<Option<&str>>,
    auto_publish: Option<bool>,
) -> Result<Option<Rule>, AppError> {
    let Some(current) = get_rule(state, id).await? else {
        return Ok(None);
    };

    let target_kind = target_kind.unwrap_or(&current.target_kind);
    let linked_repo_id = match linked_repo_id {
        Some(v) => v.map(str::to_owned),
        None => current.linked_repo_id.clone(),
    };
    let auto_publish = auto_publish.unwrap_or(current.auto_publish);

    let sql = adapt_sql(
        "UPDATE happyview_lexicon_publish_rules \
         SET target_kind = ?, linked_repo_id = ?, auto_publish = ?, updated_at = ? WHERE id = ?",
        state.db_backend,
    );
    query(&sql)
        .bind(target_kind)
        .bind(linked_repo_id)
        .bind(auto_publish)
        .bind(now_rfc3339())
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| rule_write_error("failed to update lexicon publish rule", e))?;

    get_rule(state, id).await
}

pub async fn delete_rule(state: &AppState, id: &str) -> Result<bool, AppError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_lexicon_publish_rules WHERE id = ?",
        state.db_backend,
    );
    let result = query(&sql)
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| internal("failed to delete lexicon publish rule", e))?;
    Ok(result.rows_affected() > 0)
}

pub async fn list_publications(state: &AppState) -> Result<Vec<Publication>, AppError> {
    let sql = adapt_sql(
        &format!("SELECT {PUBLICATION_COLUMNS} FROM happyview_lexicon_publications ORDER BY nsid"),
        state.db_backend,
    );
    let rows: Vec<PublicationRow> = query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| internal("failed to list lexicon publications", e))?;
    Ok(rows.into_iter().map(publication_from_row).collect())
}

pub async fn get_publication(
    state: &AppState,
    nsid: &str,
) -> Result<Option<Publication>, AppError> {
    let sql = adapt_sql(
        &format!("SELECT {PUBLICATION_COLUMNS} FROM happyview_lexicon_publications WHERE nsid = ?"),
        state.db_backend,
    );
    let row: Option<PublicationRow> = query_as(&sql)
        .bind(nsid)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| internal("failed to read lexicon publication", e))?;
    Ok(row.map(publication_from_row))
}

pub async fn record_success(
    state: &AppState,
    nsid: &str,
    did: &str,
    uri: &str,
    cid: &str,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_lexicon_publications \
         (nsid, did, uri, cid, published_at, last_error, last_attempt_at) \
         VALUES (?, ?, ?, ?, ?, NULL, ?) \
         ON CONFLICT (nsid) DO UPDATE SET did = excluded.did, uri = excluded.uri, \
         cid = excluded.cid, published_at = excluded.published_at, \
         last_error = NULL, last_attempt_at = excluded.last_attempt_at, withdrawn_at = NULL",
        state.db_backend,
    );
    query(&sql)
        .bind(nsid)
        .bind(did)
        .bind(uri)
        .bind(cid)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| internal("failed to record lexicon publication", e))?;
    Ok(())
}

/// Keeps `did`/`uri`/`cid`/`published_at`: a failed attempt does not unpublish
/// what an earlier one put on the PDS.
pub async fn record_failure(state: &AppState, nsid: &str, error: &str) -> Result<(), AppError> {
    let sql = adapt_sql(
        "INSERT INTO happyview_lexicon_publications (nsid, last_error, last_attempt_at) \
         VALUES (?, ?, ?) \
         ON CONFLICT (nsid) DO UPDATE SET last_error = excluded.last_error, \
         last_attempt_at = excluded.last_attempt_at",
        state.db_backend,
    );
    query(&sql)
        .bind(nsid)
        .bind(error)
        .bind(now_rfc3339())
        .execute(&state.db)
        .await
        .map_err(|e| internal("failed to record lexicon publication failure", e))?;
    Ok(())
}

/// Forgets where the record was, since it is gone, and marks the lexicon
/// withdrawn so auto-publish does not put it back.
pub async fn record_withdrawn(state: &AppState, nsid: &str) -> Result<(), AppError> {
    let sql = adapt_sql(
        "INSERT INTO happyview_lexicon_publications \
         (nsid, did, uri, cid, published_at, last_error, last_attempt_at, withdrawn_at) \
         VALUES (?, NULL, NULL, NULL, NULL, NULL, NULL, ?) \
         ON CONFLICT (nsid) DO UPDATE SET did = NULL, uri = NULL, cid = NULL, \
         published_at = NULL, last_error = NULL, withdrawn_at = excluded.withdrawn_at",
        state.db_backend,
    );
    query(&sql)
        .bind(nsid)
        .bind(now_rfc3339())
        .execute(&state.db)
        .await
        .map_err(|e| internal("failed to record lexicon withdrawal", e))?;
    Ok(())
}
