//! Guardrails for raw SQL surfaces that let scripts and plugins talk to the
//! database directly: Lua's `db.raw` and the plugin host's `host_db_query` /
//! `host_db_execute` imports. Both share the same table guard so protected
//! internal tables cannot be reached from either surface.

/// Table-name prefix reserved for HappyView's own internal tables. `db.raw`
/// blocks these **by default** — so a table added in a future migration is
/// protected until it is deliberately allowed — except for the data tables in
/// [`ALLOWED_INTERNAL_TABLES`].
const PROTECTED_TABLE_PREFIX: &str = "happyview_";

/// Internal tables that don't carry the `happyview_` prefix but are still
/// off-limits (SQLx's migration bookkeeping).
const PROTECTED_EXACT_TABLES: &[&str] = &["_sqlx_migrations"];

/// Internal tables `db.raw` is allowed to read and write despite the reserved
/// prefix: public AppView data and space *data*. Everything else `happyview_*`
/// stays blocked — secrets and tokens (`happyview_dpop_keys`/`_sessions`,
/// `happyview_api_keys`, `happyview_oauth_*`, `happyview_script_variables`),
/// auth/privilege state (`happyview_users`/`_user_permissions`, the delegation
/// tables), trust config (`happyview_domains`, `happyview_instance_settings`),
/// and cryptographic material (`happyview_space_credentials`, and
/// `happyview_space_repo_state` which holds commit-signature key material).
///
/// Space membership/records are exposed because a space defines *access*, not
/// confidentiality — whether to expose otherwise-private space data through the
/// AppView is left to the admin.
const ALLOWED_INTERNAL_TABLES: &[&str] = &[
    // Public AppView data.
    "happyview_records",
    "happyview_record_refs",
    "happyview_labels",
    crate::catalog::lexicon_rows::TABLE,
    // Background jobs.
    "happyview_jobs",
    // Space data (not the credential/key-material tables).
    "happyview_spaces",
    "happyview_space_members",
    "happyview_space_records",
    "happyview_space_record_oplog",
    "happyview_space_notify_registrations",
    "happyview_space_dids",
];

/// Reject a raw SQL string that references a protected internal table.
///
/// Tokenizes the SQL (so string literals and comments containing the prefix are
/// ignored, and quoted / schema-qualified identifiers are still caught) and
/// blocks any `happyview_*` (or `_sqlx_migrations`) identifier that is not in
/// [`ALLOWED_INTERNAL_TABLES`]. Unicode-escaped identifiers (`U&"…"`) are refused
/// outright as an evasion vector, and SQL that cannot be tokenized fails closed.
pub fn check_raw_sql_tables(sql: &str) -> Result<(), String> {
    use sqlparser::dialect::GenericDialect;
    use sqlparser::tokenizer::{Token, Tokenizer};

    // `U&'…'` / `U&"…"` unicode-escaped literals could smuggle a protected
    // identifier past tokenization (the escapes decode to letters); there is no
    // legitimate need for them in raw SQL, so refuse them outright.
    let lowered = sql.to_ascii_lowercase();
    if lowered.contains("u&\"") || lowered.contains("u&'") {
        return Err("raw SQL does not allow unicode-escaped identifiers".into());
    }

    // Tokenizing (rather than substring matching) means the prefix inside string
    // literals or comments is ignored, while quoted and schema-qualified
    // identifiers are still seen. SQL we cannot tokenize fails closed.
    let tokens = Tokenizer::new(&GenericDialect {}, sql)
        .tokenize()
        .map_err(|e| format!("could not parse SQL: {e}"))?;

    for token in tokens {
        if let Token::Word(word) = token {
            let name = word.value.to_ascii_lowercase();
            let is_internal = name.starts_with(PROTECTED_TABLE_PREFIX)
                || PROTECTED_EXACT_TABLES.contains(&name.as_str());
            if is_internal && !ALLOWED_INTERNAL_TABLES.contains(&name.as_str()) {
                return Err(format!(
                    "raw SQL cannot reference the protected internal HappyView table '{}'",
                    word.value
                ));
            }
        }
    }

    Ok(())
}

/// Why a raw write to a claimed collection's records is refused.
pub const CLAIMED_WRITE_REFUSED: &str = "writes to records of a collection stored outside this instance must go through records.indexPut";

/// The compat view raw SQL writes records through.
const RECORDS_VIEW: &str = "happyview_records";

/// With collections stored outside this instance (`claims` says which), refuse
/// a raw write to `happyview_records` that could create or change a local row
/// for one of them: reads of a claimed collection go to the external storage,
/// so the row would never be seen.
///
/// A write is checked by the collections it names: every string literal in
/// the statement and every string parameter, read as a collection NSID or a
/// record URI. One that is claimed refuses the write; a write naming no
/// collection at all cannot be checked, and is refused too. Read-only
/// statements, and statements that do not mention `happyview_records`, pass.
/// SQL that cannot be tokenized fails closed.
pub fn check_claimed_record_writes(
    sql: &str,
    params: &[serde_json::Value],
    claims: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    use sqlparser::dialect::GenericDialect;
    use sqlparser::tokenizer::{Token, Tokenizer};

    if is_read_only(sql) == Ok(true) {
        return Ok(());
    }
    let tokens = Tokenizer::new(&GenericDialect {}, sql)
        .tokenize()
        .map_err(|e| format!("could not parse SQL: {e}"))?;
    let writes_records = tokens.iter().any(
        |token| matches!(token, Token::Word(word) if word.value.eq_ignore_ascii_case(RECORDS_VIEW)),
    );
    if !writes_records {
        return Ok(());
    }
    let literals = tokens.iter().filter_map(|token| match token {
        Token::SingleQuotedString(s)
        | Token::EscapedStringLiteral(s)
        | Token::NationalStringLiteral(s) => Some(s.as_str()),
        Token::DollarQuotedString(s) => Some(s.value.as_str()),
        _ => None,
    });
    let params = params.iter().filter_map(serde_json::Value::as_str);
    let mut named = false;
    for collection in literals.chain(params).filter_map(named_collection) {
        if claims(collection) {
            return Err(format!("{CLAIMED_WRITE_REFUSED}: {collection}"));
        }
        named = true;
    }
    if named {
        Ok(())
    } else {
        Err(format!(
            "a write to {RECORDS_VIEW} must name its collection or record URI as a literal \
             or parameter while collections are stored outside this instance"
        ))
    }
}

/// The collection `text` names: the collection of a record URI, or `text`
/// itself when it has an NSID's shape.
fn named_collection(text: &str) -> Option<&str> {
    if text.starts_with("at://") {
        return crate::record_store::codec::parse_record_uri(text).map(|(_, c, _)| c);
    }
    let segments: Vec<&str> = text.split('.').collect();
    (segments.len() >= 3
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }))
    .then_some(text)
}

/// True when a `Query`'s body — and every CTE feeding it — is read-only.
///
/// sqlparser 0.62 parses `WITH x AS (...) DELETE/INSERT/UPDATE/MERGE ...` as a
/// `Statement::Query` whose `body` is a data-modifying `SetExpr` variant, not as
/// a `Statement::Delete`/`Insert`/etc — so checking the outer `Statement`
/// variant alone is not enough. A CTE's own body can independently be a write
/// (`WITH x AS (DELETE FROM t RETURNING uri) SELECT * FROM x`), so every
/// `with.cte_tables[].query` is checked recursively as well.
fn query_is_read_only(query: &sqlparser::ast::Query) -> bool {
    use sqlparser::ast::SetExpr;

    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            if !query_is_read_only(&cte.query) {
                return false;
            }
        }
    }

    fn set_expr_is_read_only(expr: &SetExpr) -> bool {
        match expr {
            SetExpr::Select(_) | SetExpr::Values(_) | SetExpr::Table(_) => true,
            SetExpr::Query(q) => query_is_read_only(q),
            SetExpr::SetOperation { left, right, .. } => {
                set_expr_is_read_only(left) && set_expr_is_read_only(right)
            }
            SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_) => {
                false
            }
        }
    }

    set_expr_is_read_only(&query.body)
}

/// True when every statement is a query whose body and CTEs are all
/// read-only. Parsed with the generic dialect and fails closed: SQL that
/// does not parse is not known to be read-only.
pub fn is_read_only(sql: &str) -> Result<bool, String> {
    use sqlparser::ast::Statement;
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;
    let statements = Parser::parse_sql(&GenericDialect {}, sql)
        .map_err(|e| format!("could not parse SQL: {e}"))?;
    Ok(!statements.is_empty()
        && statements.iter().all(|s| match s {
            Statement::Query(q) => query_is_read_only(q),
            _ => false,
        }))
}

/// Why a raw read that mixes record views with other relations is refused.
pub const RAW_MIXED_REFUSED: &str =
    "this query reads claimed records and local tables together; split it into two queries";

/// The compat views that serve record reads.
const RECORD_VIEWS: [&str; 2] = ["happyview_records", "happyview_record_refs"];

/// Which relations a raw query reads, as far as the record views go.
#[derive(Debug, PartialEq, Eq)]
pub enum RecordViews {
    /// None of the record views.
    None,
    /// Only the record views.
    Only,
    /// The record views together with another relation, named here.
    Mixed(String),
}

/// Which relations a read-only raw query reads: none of the record compat
/// views, only them (CTE names and table functions don't count), or them
/// together with another relation. SQL that names a view but does not parse
/// fails closed.
pub fn record_view_use(sql: &str) -> Result<RecordViews, String> {
    use sqlparser::ast::Visit;
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;
    use sqlparser::tokenizer::{Token, Tokenizer};

    // Cheap pre-check on tokens, so SQL that never names a view is never parsed.
    let dialect = GenericDialect {};
    let names_view = match Tokenizer::new(&dialect, sql).tokenize() {
        Ok(tokens) => tokens.iter().any(|token| {
            matches!(token, Token::Word(word)
                if RECORD_VIEWS.contains(&word.value.to_ascii_lowercase().as_str()))
        }),
        Err(_) => {
            let lowered = sql.to_ascii_lowercase();
            RECORD_VIEWS.iter().any(|view| lowered.contains(view))
        }
    };
    if !names_view {
        return Ok(RecordViews::None);
    }

    let statements = Parser::parse_sql(&dialect, sql)
        .map_err(|e| format!("raw SQL that reads record views must parse: {e}"))?;
    let mut relations = Relations::default();
    for statement in &statements {
        let _ = statement.visit(&mut relations);
    }

    let mut saw_view = false;
    for name in relations.tables {
        if relations.ctes.contains(&name) {
            continue;
        }
        if RECORD_VIEWS.contains(&name.as_str()) {
            saw_view = true;
        } else {
            return Ok(RecordViews::Mixed(name));
        }
    }
    Ok(if saw_view {
        RecordViews::Only
    } else {
        RecordViews::None
    })
}

#[derive(Default)]
struct Relations {
    tables: Vec<String>,
    ctes: std::collections::HashSet<String>,
}

impl sqlparser::ast::Visitor for Relations {
    type Break = ();

    fn pre_visit_query(&mut self, query: &sqlparser::ast::Query) -> std::ops::ControlFlow<()> {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.ctes.insert(cte.alias.name.value.to_ascii_lowercase());
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(
        &mut self,
        factor: &sqlparser::ast::TableFactor,
    ) -> std::ops::ControlFlow<()> {
        // `args: Some(..)` is a table function (`json_each(..)`), not a relation.
        if let sqlparser::ast::TableFactor::Table {
            name, args: None, ..
        } = factor
            && let Some(sqlparser::ast::ObjectNamePart::Identifier(last)) = name.0.last()
        {
            self.tables.push(last.value.to_ascii_lowercase());
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_view_use_classifies_relations() {
        use RecordViews::*;
        let cases: &[(&str, &str)] = &[
            ("SELECT 1", "none"),
            ("SELECT * FROM my_table", "none"),
            (
                "SELECT * FROM happyview_records WHERE collection = 'a.b.c'",
                "only",
            ),
            (
                "SELECT r.uri FROM happyview_records r JOIN happyview_record_refs f ON f.source_uri = r.uri",
                "only",
            ),
            ("SELECT * FROM public.happyview_records", "only"),
            (
                "WITH x AS (SELECT * FROM happyview_records) SELECT * FROM x",
                "only",
            ),
            (
                "SELECT * FROM happyview_records r, json_each(r.record)",
                "only",
            ),
            (
                "SELECT * FROM happyview_records r JOIN my_table m ON m.uri = r.uri",
                "mixed:my_table",
            ),
            (
                "SELECT * FROM happyview_records WHERE uri IN (SELECT uri FROM happyview_labels)",
                "mixed:happyview_labels",
            ),
            (
                "SELECT (SELECT COUNT(*) FROM happyview_records), (SELECT COUNT(*) FROM notes)",
                "mixed:notes",
            ),
        ];
        for (sql, want) in cases {
            let got = match record_view_use(sql).unwrap() {
                None => "none".to_string(),
                Only => "only".to_string(),
                Mixed(t) => format!("mixed:{t}"),
            };
            assert_eq!(&got, want, "{sql}");
        }
    }

    #[test]
    fn record_view_use_fails_closed_on_unparseable_view_queries() {
        assert!(record_view_use("SELECT * FROM happyview_records WHERE (").is_err());
        // no view token: never parsed, never an error
        assert!(matches!(
            record_view_use("NOT SQL AT ALL"),
            Ok(RecordViews::None)
        ));
    }

    #[test]
    fn raw_sql_allows_non_protected_tables() {
        // Admins can get wild with their own tables.
        assert!(super::check_raw_sql_tables("SELECT * FROM my_table").is_ok());
        assert!(super::check_raw_sql_tables("CREATE TABLE analytics (id INT)").is_ok());
        assert!(super::check_raw_sql_tables("INSERT INTO analytics VALUES (1)").is_ok());
        assert!(super::check_raw_sql_tables("UPDATE analytics SET id = 2").is_ok());
        assert!(super::check_raw_sql_tables("DELETE FROM analytics WHERE id = 1").is_ok());
        assert!(super::check_raw_sql_tables("DROP TABLE analytics").is_ok());
        // A table that merely *contains* the prefix mid-name is fine.
        assert!(super::check_raw_sql_tables("SELECT * FROM myhappyview_data").is_ok());
        // The prefix appearing inside a string literal is not a table reference.
        assert!(
            super::check_raw_sql_tables("INSERT INTO logs (msg) VALUES ('happyview_started')")
                .is_ok()
        );
    }

    #[test]
    fn raw_sql_allows_allowlisted_internal_tables() {
        // Public AppView data and background jobs are readable/writable.
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_records").is_ok());
        assert!(
            super::check_raw_sql_tables("DELETE FROM happyview_records WHERE uri = $1").is_ok()
        );
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_record_refs").is_ok());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_labels").is_ok());
        assert!(
            super::check_raw_sql_tables(&format!(
                "SELECT * FROM {}",
                crate::catalog::lexicon_rows::TABLE
            ))
            .is_ok()
        );
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_jobs").is_ok());
        // Space data (access, not confidentiality).
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_space_records").is_ok());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_space_members").is_ok());
    }

    #[test]
    fn raw_sql_blocks_protected_tables() {
        // Secrets / tokens / keys.
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_dpop_keys").is_err());
        assert!(super::check_raw_sql_tables("DROP TABLE happyview_api_keys").is_err());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_script_variables").is_err());
        // Auth / privilege / trust config.
        assert!(super::check_raw_sql_tables("UPDATE happyview_users SET is_super = true").is_err());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_domains").is_err());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_instance_settings").is_err());
        // Space credential / key-material tables stay blocked even though other
        // space tables are allowed.
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_space_credentials").is_err());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_space_repo_state").is_err());
        // The migration bookkeeping table is off-limits too.
        assert!(super::check_raw_sql_tables("SELECT * FROM _sqlx_migrations").is_err());
    }

    #[test]
    fn raw_sql_blocks_protected_tables_evasion() {
        // Case-insensitive.
        assert!(super::check_raw_sql_tables("SELECT * FROM HAPPYVIEW_USERS").is_err());
        // Double-quoted identifier.
        assert!(super::check_raw_sql_tables(r#"SELECT * FROM "happyview_api_keys""#).is_err());
        // Schema-qualified.
        assert!(
            super::check_raw_sql_tables("SELECT * FROM public.happyview_dpop_sessions").is_err()
        );
        // Second statement in a batch.
        assert!(super::check_raw_sql_tables("SELECT 1; SELECT * FROM happyview_users").is_err());
        // JOIN / subquery position.
        assert!(
            super::check_raw_sql_tables(
                "SELECT * FROM my_table JOIN happyview_api_clients USING (id)"
            )
            .is_err()
        );
        // Unicode-escaped identifier evasion is refused outright.
        assert!(super::check_raw_sql_tables(r#"SELECT * FROM U&"happyview_dpop_keys""#).is_err());
    }

    #[test]
    fn read_only_accepts_selects_and_ctes() {
        assert_eq!(is_read_only("SELECT 1"), Ok(true));
        assert_eq!(
            is_read_only("WITH x AS (SELECT 1) SELECT * FROM x"),
            Ok(true)
        );
        assert_eq!(is_read_only("SELECT a FROM t; SELECT b FROM u"), Ok(true));
    }

    #[test]
    fn read_only_rejects_writes_even_after_a_select() {
        assert_eq!(is_read_only("INSERT INTO t VALUES (1)"), Ok(false));
        assert_eq!(is_read_only("SELECT 1; DELETE FROM t"), Ok(false));
        assert_eq!(is_read_only("DROP TABLE t"), Ok(false));
        // Quoted: sqlparser 0.62.0 only accepts a literal value after `PRAGMA
        // name =`, not a bare identifier, so an unquoted `= DELETE` fails to
        // parse at all rather than parsing as a (non-Query) Pragma statement.
        assert_eq!(is_read_only("PRAGMA journal_mode = 'DELETE'"), Ok(false));
    }

    #[test]
    fn read_only_rejects_writes_hidden_in_query_statements() {
        // sqlparser parses these as Statement::Query with a write SetExpr body,
        // not as Statement::Delete/Insert/Update — a naive
        // `matches!(s, Statement::Query(_))` check would wrongly accept them.
        assert_eq!(
            is_read_only("WITH x AS (SELECT 1) DELETE FROM happyview_records"),
            Ok(false)
        );
        assert_eq!(
            is_read_only("WITH x AS (SELECT 1) INSERT INTO happyview_records VALUES (1)"),
            Ok(false)
        );
        assert_eq!(
            is_read_only("WITH x AS (SELECT 1) UPDATE happyview_records SET uri = 'a'"),
            Ok(false)
        );
        // A write hidden inside the CTE body itself, not the outer statement.
        assert_eq!(
            is_read_only("WITH x AS (DELETE FROM happyview_records RETURNING uri) SELECT * FROM x"),
            Ok(false)
        );
    }

    #[test]
    fn read_only_accepts_ctes_and_set_operations() {
        assert_eq!(
            is_read_only("WITH x AS (SELECT 1) SELECT * FROM x UNION SELECT 2"),
            Ok(true)
        );
        assert_eq!(is_read_only("SELECT 1 UNION ALL SELECT 2"), Ok(true));
    }

    #[test]
    fn claimed_record_writes_are_refused_and_others_pass() {
        let claims = |c: &str| c == "app.claimed.thing";
        let check = |sql: &str, params: &[serde_json::Value]| {
            check_claimed_record_writes(sql, params, &claims)
        };
        // Reads, and writes elsewhere, pass whatever they name.
        assert!(
            check(
                "SELECT * FROM happyview_records WHERE collection = 'app.claimed.thing'",
                &[]
            )
            .is_ok()
        );
        assert!(check("DELETE FROM my_table", &[]).is_ok());
        // A claimed collection, as a literal, a URI or a parameter.
        for (sql, params) in [
            (
                "INSERT INTO happyview_records (collection) VALUES ('app.claimed.thing')",
                vec![],
            ),
            (
                "DELETE FROM happyview_records WHERE uri = 'at://did:plc:a/app.claimed.thing/1'",
                vec![],
            ),
            (
                "UPDATE happyview_records SET record = ? WHERE collection = 'app.local.thing' OR collection = ?",
                vec![
                    serde_json::json!("{}"),
                    serde_json::json!("app.claimed.thing"),
                ],
            ),
        ] {
            let err = check(sql, &params).unwrap_err();
            assert!(err.starts_with(CLAIMED_WRITE_REFUSED), "{err}");
            assert!(err.ends_with("app.claimed.thing"), "{err}");
        }
        // A local collection, by literal or parameter.
        assert!(
            check(
                "DELETE FROM happyview_records WHERE collection = 'app.local.thing'",
                &[]
            )
            .is_ok()
        );
        assert!(
            check(
                "DELETE FROM HAPPYVIEW_RECORDS WHERE uri = ?",
                &[serde_json::json!("at://did:plc:a/app.local.thing/1")]
            )
            .is_ok()
        );
        // Nothing to check against: refused.
        assert!(check("DELETE FROM happyview_records WHERE did = 'did:plc:a'", &[]).is_err());
        assert!(check("DELETE FROM happyview_records", &[]).is_err());
    }

    #[test]
    fn read_only_fails_closed_on_unparseable_sql() {
        // "SELECT FROM WHERE" is not actually a parse error under sqlparser's
        // generic dialect ("WHERE" reads as a table name), so this uses SQL
        // that genuinely cannot be parsed as any statement.
        assert!(is_read_only("SELECT 1 FROM (").is_err());
    }
}
