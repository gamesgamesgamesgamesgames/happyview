//! Startup repair for `happyview_lexicons` rows whose id is not a usable NSID.
//!
//! The admin API validated nothing about a lexicon's `id` before issue #86, and
//! the dashboard's boilerplate ships with `"id": ""`. Uploading it as-is stored
//! a row that no longer has any route to it: `DELETE /admin/lexicons/{id}`
//! collapses to `/admin/lexicons/`, which matches no route, so the row is
//! permanently stuck — and if it is a record lexicon it also contributes an
//! empty `wantedCollections` value to the Jetstream subscription.
//!
//! The sweep is deliberately narrow. It deletes only rows the admin API cannot
//! reach, and merely warns about ids that are malformed but still addressable:
//! those are the operator's to keep or remove, and silently deleting a lexicon
//! that works would be worse than the bug this fixes.

use sqlx::AnyPool;
use tracing::{info, warn};

use crate::db::{DatabaseBackend, adapt_sql};

/// What a stored `happyview_lexicons.id` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexiconIdVerdict {
    /// A well-formed NSID.
    Valid,
    /// Not a valid NSID, but still reachable through the admin API — reported,
    /// never deleted.
    Invalid(String),
    /// Blank. An empty id is literally unreachable through `DELETE
    /// /admin/lexicons/{id}`; a whitespace-only one is technically reachable
    /// but indistinguishable from it everywhere it is displayed, and is
    /// treated the same.
    Unaddressable,
}

/// Classify a single stored lexicon id. Blankness is checked before NSID
/// validity because the two call for different handling, and a blank id fails
/// both.
pub fn classify_lexicon_id(id: &str) -> LexiconIdVerdict {
    if id.trim().is_empty() {
        return LexiconIdVerdict::Unaddressable;
    }
    match happyview_nsid::validate_nsid(id) {
        Ok(()) => LexiconIdVerdict::Valid,
        Err(e) => LexiconIdVerdict::Invalid(e.to_string()),
    }
}

/// Delete every unaddressable lexicon row and warn about malformed-but-reachable
/// ones. Returns the number of rows deleted.
///
/// Fail-open, like [`super::nsid_audit`]: a database error is logged and treated
/// as "nothing found" so a hiccup at boot cannot stop the process from booting.
pub async fn run(pool: &AnyPool, backend: DatabaseBackend) -> usize {
    let sql = adapt_sql("SELECT id FROM happyview_lexicons", backend);
    let rows: Vec<(String,)> = match crate::db::query_as(&sql).fetch_all(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "lexicon id sweep: failed to list lexicons; skipping");
            return 0;
        }
    };

    let mut removed = 0;
    for (id,) in rows {
        match classify_lexicon_id(&id) {
            LexiconIdVerdict::Valid => {}
            LexiconIdVerdict::Invalid(error) => {
                warn!(
                    lexicon_id = %id,
                    error = %error,
                    "lexicon '{}' does not have a valid NSID ({}). It keeps working and can \
                     still be deleted, but it can no longer be re-uploaded.",
                    id, error
                );
            }
            LexiconIdVerdict::Unaddressable => {
                // Bound the delete to blank ids in SQL as well as in Rust, so a
                // classifier change can never widen what this statement removes.
                let delete = adapt_sql(
                    "DELETE FROM happyview_lexicons WHERE id = ? AND trim(id) = ''",
                    backend,
                );
                match crate::db::query(&delete).bind(&id).execute(pool).await {
                    Ok(result) => {
                        let affected = result.rows_affected();
                        if affected > 0 {
                            removed += affected as usize;
                            info!(
                                "removed a lexicon with a blank id: it could not be deleted \
                                 through the admin API, and an empty NSID is not a collection \
                                 Jetstream can filter on (issue #86)"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "lexicon id sweep: failed to delete a blank-id lexicon");
                    }
                }
            }
        }
    }

    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_nsid_is_valid() {
        assert_eq!(
            classify_lexicon_id("games.gamesgamesgamesgames.game"),
            LexiconIdVerdict::Valid
        );
    }

    #[test]
    fn an_empty_id_is_unaddressable() {
        assert_eq!(classify_lexicon_id(""), LexiconIdVerdict::Unaddressable);
    }

    #[test]
    fn a_blank_id_is_unaddressable() {
        assert_eq!(classify_lexicon_id("   "), LexiconIdVerdict::Unaddressable);
    }

    #[test]
    fn a_malformed_but_addressable_id_is_invalid_not_unaddressable() {
        // Two segments: rejected on upload today, but `DELETE
        // /admin/lexicons/com.example` still reaches it, so it is the
        // operator's to remove, not this sweep's.
        let verdict = classify_lexicon_id("com.example");
        assert!(
            matches!(verdict, LexiconIdVerdict::Invalid(ref e) if e.contains("com.example")),
            "expected Invalid naming the id, got {verdict:?}"
        );
    }
}
