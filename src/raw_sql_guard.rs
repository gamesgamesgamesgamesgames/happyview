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
/// prefix: public AppView data and background jobs. Everything else
/// `happyview_*` stays blocked — secrets and tokens (`happyview_dpop_keys`/
/// `_sessions`, `happyview_api_keys`, `happyview_oauth_*`,
/// `happyview_script_variables`), auth/privilege state
/// (`happyview_users`/`_user_permissions`, the delegation tables), trust config
/// (`happyview_domains`, `happyview_instance_settings`), cryptographic
/// material, and every space table.
///
/// Space tables are reached only through `happyview.spaces`, which applies the
/// space's access rules for its caller. Raw SQL reads and writes rows
/// directly, outside those rules.
const ALLOWED_INTERNAL_TABLES: &[&str] = &[
    // Public AppView data.
    "happyview_records",
    "happyview_record_refs",
    "happyview_records_by_collection",
    "happyview_labels",
    crate::catalog::lexicon_rows::TABLE,
    // Background jobs.
    "happyview_jobs",
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

/// The internal relations a plugin's raw SQL may name, and the relations the
/// least-privileged Postgres roles are granted. One source of truth so the
/// parser allowlist in [`check_postgres_plugin_sql`] and the role grants in
/// `plugin::host::db` cannot drift apart.
pub fn plugin_nameable_relations() -> &'static [&'static str] {
    ALLOWED_INTERNAL_TABLES
}

/// Whether a plugin holds only `database:read` or `database:write`. Chooses the
/// statement kinds the parser allowlist accepts and, on Postgres, the role the
/// statement runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginSqlMode {
    Read,
    Write,
}

/// How a Postgres plugin statement is isolated at the database — which decides
/// how much the parser must enforce on top.
///
/// - `LoginPool`: a separate connection that logs in **as** a least-privileged
///   role, so `session_user` itself has no access to the protected tables and
///   there is no privileged role to switch back to. The role is the boundary, so
///   the function allowlist is not needed (the deny-list still applies).
/// - `Role`: the main (privileged) connection, restricted per statement with
///   `SET LOCAL ROLE`. `session_user` is still privileged, so the function
///   allowlist is applied as well, to stop a statement reaching a
///   privilege-regaining or SQL-text-executing built-in.
/// - `ParserOnly`: the roles could not be set up; the main privileged connection
///   with no role change. The parser is the whole boundary: DML only, the
///   function allowlist, and relations restricted to the `public` schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginSqlEnforcement {
    LoginPool,
    Role,
    ParserOnly,
}

impl PluginSqlEnforcement {
    fn allows_ddl(self) -> bool {
        self != PluginSqlEnforcement::ParserOnly
    }
    fn applies_function_allowlist(self) -> bool {
        self != PluginSqlEnforcement::LoginPool
    }
    fn restricts_to_public_schema(self) -> bool {
        self == PluginSqlEnforcement::ParserOnly
    }
}

/// True when `name` is one of HappyView's own internal tables that plugin raw
/// SQL may not reach — the same rule as [`check_raw_sql_tables`], but applied to
/// a name the real parser extracted. A caller-owned table (anything without the
/// reserved prefix) is not blocked; plugins are expected to use their own
/// tables through raw SQL.
fn is_blocked_internal_table(name: &str) -> bool {
    let lname = name.to_ascii_lowercase();
    let internal = lname.starts_with(PROTECTED_TABLE_PREFIX)
        || PROTECTED_EXACT_TABLES.contains(&lname.as_str());
    internal
        && !ALLOWED_INTERNAL_TABLES
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&lname))
}

/// Built-ins refused in **every** mode, including the login pool — defense in
/// depth on top of the role boundary. Three groups:
///
/// - the function form of `SET` (`set_config`), which could change the session
///   role or search_path from inside a statement;
/// - functions that execute a SQL string passed as text, which would run that
///   text without it passing the parser;
/// - functions that leave session state the per-statement `SET LOCAL` does not
///   undo — session advisory locks and large objects — so nothing a plugin does
///   outlives its statement on a pooled connection;
/// - functions that read server files or open outbound connections.
///
/// This is a deny-list and so is kept narrow and specific; it backs the role and
/// (outside the login pool) the function allowlist, which are the real
/// boundaries.
const ALWAYS_FORBIDDEN_FUNCTIONS: &[&str] = &[
    // The function form of SET.
    "set_config",
    // Execute a SQL string given as text.
    "query_to_xml",
    "query_to_xmlschema",
    "query_to_xml_and_xmlschema",
    "cursor_to_xml",
    "cursor_to_xmlschema",
    "ts_stat",
    "ts_rewrite",
    "dblink",
    "dblink_exec",
    "dblink_connect",
    "dblink_connect_u",
    // Session-lifetime state that `SET LOCAL` does not release.
    "pg_advisory_lock",
    "pg_advisory_lock_shared",
    "pg_advisory_unlock",
    "pg_advisory_unlock_shared",
    "pg_advisory_unlock_all",
    "pg_try_advisory_lock",
    "pg_try_advisory_lock_shared",
    "lo_import",
    "lo_export",
    "lo_create",
    "lo_creat",
    "lo_unlink",
    "lo_from_bytea",
    "lo_put",
    "lo_get",
    "lo_open",
    "lowrite",
    "loread",
    // Server files.
    "pg_read_file",
    "pg_read_binary_file",
    "pg_ls_dir",
    "pg_stat_file",
];

/// True when a write-mode statement is of a kind plugin raw SQL may run.
///
/// DML writes are always allowed. Table-shaped DDL (`CREATE`/`ALTER`/`DROP`
/// TABLE, INDEX, VIEW) is allowed only when `allow_ddl` — i.e. when the
/// least-privileged roles are in force. Without them the parser is the whole
/// boundary, and DDL is refused outright as a conservative simplification (the
/// generic tree scan does catch a function in a column `DEFAULT`/`CHECK`/
/// `GENERATED` expression, so this is belt-and-braces, not a required cover).
/// Under the roles those expressions run as the restricted role in any case.
///
/// This is an **allowlist of statement kinds**, so an unlisted or future kind
/// fails closed. It leaves out every role/session/privilege and code-defining
/// kind — `DO`, `CREATE FUNCTION`/`OPERATOR`/`CAST`/`AGGREGATE`/`TRIGGER`/
/// `RULE`/`EXTENSION`/`LANGUAGE`/`EVENT TRIGGER`, `COPY`, `EXECUTE`/`PREPARE`,
/// `GRANT`/`REVOKE`, the role statements, `SET`/`RESET`, and transaction
/// control — so a plugin cannot define a database object that HappyView's own
/// queries could resolve to. `ALTER TABLE … OWNER TO` is refused even though
/// `ALTER TABLE` is allowed.
fn is_allowed_write_statement(
    node: Option<&pg_query::protobuf::node::Node>,
    allow_ddl: bool,
) -> bool {
    use pg_query::protobuf::node::Node;
    if statement_creates_temp_or_into(node) {
        // A temp object (or `SELECT … INTO`) outlives `SET LOCAL` on the pooled
        // connection and, because it lands in `pg_temp` which resolves first,
        // could shadow a name HappyView's own queries use. Refused in every mode.
        return false;
    }
    match node {
        Some(
            Node::SelectStmt(_)
            | Node::InsertStmt(_)
            | Node::UpdateStmt(_)
            | Node::DeleteStmt(_)
            | Node::MergeStmt(_),
        ) => true,
        Some(Node::CreateStmt(_) | Node::IndexStmt(_) | Node::ViewStmt(_) | Node::DropStmt(_)) => {
            allow_ddl
        }
        Some(Node::AlterTableStmt(s)) => allow_ddl && !alter_table_changes_owner(s),
        _ => false,
    }
}

/// True when a `RangeVar`'s `relpersistence` marks it a temporary object.
fn is_temp_relation(rel: Option<&pg_query::protobuf::RangeVar>) -> bool {
    rel.is_some_and(|r| r.relpersistence == "t")
}

/// True when the statement creates a temporary object, or is `SELECT … INTO`
/// (which creates a table). Refused in every mode (see
/// [`is_allowed_write_statement`]); also checked for a bare `SELECT` read.
fn statement_creates_temp_or_into(node: Option<&pg_query::protobuf::node::Node>) -> bool {
    use pg_query::protobuf::node::Node;
    match node {
        Some(Node::SelectStmt(s)) => s.into_clause.is_some(),
        Some(Node::CreateStmt(s)) => is_temp_relation(s.relation.as_ref()),
        Some(Node::ViewStmt(s)) => is_temp_relation(s.view.as_ref()),
        Some(Node::CreateTableAsStmt(s)) => s
            .into
            .as_ref()
            .is_some_and(|into| is_temp_relation(into.rel.as_ref())),
        _ => false,
    }
}

/// True when an `ALTER TABLE` includes an `OWNER TO` sub-command. Changing a
/// table's owner is a privilege operation, refused even though the rest of
/// `ALTER TABLE` is allowed.
fn alter_table_changes_owner(stmt: &pg_query::protobuf::AlterTableStmt) -> bool {
    use pg_query::protobuf::node::Node;
    // `AlterTableType::AtChangeOwner` is 28.
    const AT_CHANGE_OWNER: i32 = 28;
    stmt.cmds.iter().any(|cmd| {
        matches!(
            cmd.node.as_ref(),
            Some(Node::AlterTableCmd(c)) if c.subtype == AT_CHANGE_OWNER || c.newowner.is_some()
        )
    })
}

/// Protected internal names a `DROP` or `CREATE INDEX` must not target. `DROP`'s
/// object names and a new index's name are not `RangeVar`s, so the generic
/// relation scan does not see them; check them here.
fn ddl_targets_protected_name(node: Option<&pg_query::protobuf::node::Node>) -> bool {
    use pg_query::protobuf::node::Node;
    match node {
        Some(Node::IndexStmt(s)) => is_blocked_internal_table(&s.idxname),
        Some(Node::DropStmt(s)) => s.objects.iter().any(|obj| {
            // Each object is a `List` of `String` name parts; the last is the
            // object's own name.
            drop_object_names(obj)
                .last()
                .is_some_and(|name| is_blocked_internal_table(name))
        }),
        _ => false,
    }
}

/// True when `name` is in HappyView's reserved namespace — any `happyview_`
/// prefix (or `_sqlx_migrations`), whether or not it is on the read allowlist. A
/// plugin may not *create* or *rename to* such a name, even an allowlisted one,
/// so another plugin's read of that name can't resolve to the plugin's object in
/// the plugin schema (which precedes `public` on the plugin roles' path).
fn is_reserved_object_name(name: &str) -> bool {
    let lname = name.to_ascii_lowercase();
    lname.starts_with(PROTECTED_TABLE_PREFIX) || PROTECTED_EXACT_TABLES.contains(&lname.as_str())
}

/// The bare name a create/rename statement introduces (not its schema), for the
/// reserved-name check. `None` for statements that introduce no such name.
fn introduced_object_name(node: Option<&pg_query::protobuf::node::Node>) -> Option<&str> {
    use pg_query::protobuf::node::Node as N;
    let name = match node {
        Some(N::CreateStmt(s)) => s.relation.as_ref().map(|r| r.relname.as_str()),
        Some(N::ViewStmt(s)) => s.view.as_ref().map(|r| r.relname.as_str()),
        Some(N::CreateSeqStmt(s)) => s.sequence.as_ref().map(|r| r.relname.as_str()),
        Some(N::CompositeTypeStmt(s)) => s.typevar.as_ref().map(|r| r.relname.as_str()),
        Some(N::CreateTableAsStmt(s)) => s
            .into
            .as_ref()
            .and_then(|i| i.rel.as_ref())
            .map(|r| r.relname.as_str()),
        Some(N::IndexStmt(s)) => Some(s.idxname.as_str()),
        Some(N::CreateTrigStmt(s)) => Some(s.trigname.as_str()),
        Some(N::RenameStmt(s)) => Some(s.newname.as_str()),
        Some(N::CreateFunctionStmt(s)) => last_name_part(&s.funcname),
        Some(N::CreateEnumStmt(s)) => last_name_part(&s.type_name),
        Some(N::CreateRangeStmt(s)) => last_name_part(&s.type_name),
        _ => None,
    };
    name.filter(|n| !n.is_empty())
}

/// The last `String` part of a qualified name node list (the object's own name).
fn last_name_part(nodes: &[pg_query::protobuf::Node]) -> Option<&str> {
    use pg_query::protobuf::node::Node;
    nodes.iter().rev().find_map(|n| match n.node.as_ref() {
        Some(Node::String(s)) => Some(s.sval.as_str()),
        _ => None,
    })
}

/// The dotted name parts of one `DROP` object entry.
fn drop_object_names(obj: &pg_query::protobuf::Node) -> Vec<&str> {
    use pg_query::protobuf::node::Node;
    let mut parts = Vec::new();
    if let Some(Node::List(list)) = obj.node.as_ref() {
        for item in &list.items {
            if let Some(Node::String(s)) = item.node.as_ref() {
                parts.push(s.sval.as_str());
            }
        }
    } else if let Some(Node::String(s)) = obj.node.as_ref() {
        parts.push(s.sval.as_str());
    }
    parts
}

/// The schema the plugin's own objects live in.
const PLUGIN_SCHEMA: &str = "happyview_plugin";

/// True when a schema a DDL statement names is the plugin schema or is left
/// unqualified (so it resolves to the plugin schema, first on the role's path).
fn schema_is_plugin_or_unqualified(schema: &str) -> bool {
    schema.is_empty() || schema.eq_ignore_ascii_case(PLUGIN_SCHEMA)
}

/// The explicit schema of a name node list (`schema.name` → `schema`), or empty.
fn schema_of_name_nodes(nodes: &[pg_query::protobuf::Node]) -> &str {
    use pg_query::protobuf::node::Node;
    let parts: Vec<&str> = nodes
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(Node::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect();
    if parts.len() >= 2 {
        parts[parts.len() - 2]
    } else {
        ""
    }
}

fn rangevar_schema(rv: Option<&pg_query::protobuf::RangeVar>) -> &str {
    rv.map(|r| r.schemaname.as_str()).unwrap_or("")
}

/// The schema a DDL statement's created/altered/dropped object is in, when the
/// statement is one whose object is confined to the plugin schema. `None` for a
/// statement with no single such target (DML, or a kind handled elsewhere).
fn ddl_object_schema(node: Option<&pg_query::protobuf::node::Node>) -> Option<&str> {
    use pg_query::protobuf::node::Node as N;
    match node {
        Some(N::CreateStmt(s)) => Some(rangevar_schema(s.relation.as_ref())),
        Some(N::ViewStmt(s)) => Some(rangevar_schema(s.view.as_ref())),
        Some(N::IndexStmt(s)) => Some(rangevar_schema(s.relation.as_ref())),
        Some(N::AlterTableStmt(s)) => Some(rangevar_schema(s.relation.as_ref())),
        Some(N::CreateSeqStmt(s)) => Some(rangevar_schema(s.sequence.as_ref())),
        Some(N::CreateTableAsStmt(s)) => Some(
            s.into
                .as_ref()
                .map(|i| rangevar_schema(i.rel.as_ref()))
                .unwrap_or(""),
        ),
        Some(N::CompositeTypeStmt(s)) => Some(rangevar_schema(s.typevar.as_ref())),
        Some(N::CreateEnumStmt(s)) => Some(schema_of_name_nodes(&s.type_name)),
        Some(N::CreateRangeStmt(s)) => Some(schema_of_name_nodes(&s.type_name)),
        Some(N::CreateFunctionStmt(s)) => Some(schema_of_name_nodes(&s.funcname)),
        Some(N::CreateTrigStmt(s)) => Some(rangevar_schema(s.relation.as_ref())),
        Some(N::AlterFunctionStmt(s)) => Some(
            s.func
                .as_ref()
                .map(|f| schema_of_name_nodes(&f.objname))
                .unwrap_or(""),
        ),
        _ => None,
    }
}

/// True when a `CREATE TRIGGER` attaches to one of HappyView's own relations
/// (anything with the reserved prefix, including the allowlisted views). A
/// plugin may trigger only on its own tables.
fn trigger_targets_happyview_relation(node: Option<&pg_query::protobuf::node::Node>) -> bool {
    use pg_query::protobuf::node::Node;
    matches!(node, Some(Node::CreateTrigStmt(s))
    if s.relation.as_ref().is_some_and(|r| {
        r.relname.to_ascii_lowercase().starts_with(PROTECTED_TABLE_PREFIX)
    }))
}

/// Object kinds the **login pool** additionally allows a plugin to define in its
/// own schema: functions, procedures, triggers, sequences, types and
/// `CREATE TABLE AS`. Only in the login pool, because in any mode where
/// `session_user` is still privileged a plpgsql body could `RESET ROLE` out.
/// `SECURITY DEFINER` and untrusted languages are refused; the schema and
/// trigger-target checks are applied by the caller.
fn login_pool_object_allowed(node: Option<&pg_query::protobuf::node::Node>) -> bool {
    use pg_query::protobuf::node::Node as N;
    match node {
        Some(N::CreateFunctionStmt(s)) => create_function_is_trusted(s),
        Some(N::CreateTrigStmt(s)) => {
            schema_is_plugin_or_unqualified(schema_of_name_nodes(&s.funcname))
        }
        Some(N::AlterFunctionStmt(s)) => alter_function_is_safe(s),
        Some(N::CreateSeqStmt(_))
        | Some(N::CompositeTypeStmt(_))
        | Some(N::CreateEnumStmt(_))
        | Some(N::CreateRangeStmt(_)) => true,
        Some(N::CreateTableAsStmt(_)) => !statement_creates_temp_or_into(node),
        _ => false,
    }
}

/// True when a `DefElem` option is `SECURITY DEFINER`.
fn defelem_is_security_definer(def: &pg_query::protobuf::DefElem) -> bool {
    use pg_query::protobuf::node::Node;
    def.defname.eq_ignore_ascii_case("security")
        && def
            .arg
            .as_ref()
            .is_some_and(|a| matches!(a.node.as_ref(), Some(Node::Boolean(b)) if b.boolval))
}

/// True when a `CREATE FUNCTION`/`PROCEDURE` is safe for a plugin: not
/// `SECURITY DEFINER` (a plugin's own function never needs it) and written in a
/// trusted language (`sql`/`plpgsql`), not one that loads native code.
fn create_function_is_trusted(stmt: &pg_query::protobuf::CreateFunctionStmt) -> bool {
    use pg_query::protobuf::node::Node;
    let mut language = "plpgsql"; // the default when none is named
    for opt in &stmt.options {
        if let Some(Node::DefElem(def)) = opt.node.as_ref() {
            if defelem_is_security_definer(def) {
                return false;
            }
            if def.defname.eq_ignore_ascii_case("language")
                && let Some(arg) = def.arg.as_ref()
                && let Some(Node::String(s)) = arg.node.as_ref()
            {
                language = s.sval.as_str();
            }
        }
    }
    matches!(language.to_ascii_lowercase().as_str(), "sql" | "plpgsql")
}

/// True when an `ALTER FUNCTION` is safe for a plugin: it may not set
/// `SECURITY DEFINER`, nor add a `SET` action (which could pin a search_path or
/// other session config on the function). Matches the refusals on `CREATE`.
fn alter_function_is_safe(stmt: &pg_query::protobuf::AlterFunctionStmt) -> bool {
    use pg_query::protobuf::node::Node;
    !stmt.actions.iter().any(|action| {
        matches!(action.node.as_ref(), Some(Node::DefElem(def))
            if defelem_is_security_definer(def) || def.defname.eq_ignore_ascii_case("set"))
    })
}

/// Functions plugin raw SQL may call on Postgres **when the least-privileged
/// roles are not available** and the parser is the whole boundary. An
/// **allowlist**: every function the real parser sees must be here, so any
/// built-in not on the list is refused by default rather than hunted for in a
/// deny-list. Operators and `CAST`/`::` are not function calls in the parse
/// tree, so they are allowed without listing; the risk in a cast is never the
/// cast but the function that consumes its result, which this list gates.
///
/// When the roles *are* available this list is not consulted: the role, which
/// has no grant on the protected tables, is the boundary at the database, so a
/// plugin keeps full use of functions over its own tables. Extend it on request
/// when a plugin needs a demonstrably-safe function in the fallback; keep
/// anything that reads files, runs a string as SQL, opens a connection, or
/// touches server state out of it.
const PLUGIN_SQL_FUNCTIONS: &[&str] = &[
    // Aggregates.
    "count",
    "sum",
    "avg",
    "min",
    "max",
    // Null / conditional helpers that do parse as calls.
    "coalesce",
    "nullif",
    "greatest",
    "least",
    // Strings.
    "lower",
    "upper",
    "length",
    "char_length",
    "trim",
    "btrim",
    "ltrim",
    "rtrim",
    "substr",
    "substring",
    "left",
    "right",
    "replace",
    "concat",
    "concat_ws",
    "split_part",
    "position",
    "strpos",
    "starts_with",
    "format",
    "md5",
    "encode",
    "regexp_replace",
    // Numbers.
    "abs",
    "round",
    "ceil",
    "ceiling",
    "floor",
    "mod",
    "power",
    "sign",
    // Time.
    "now",
    "statement_timestamp",
    "clock_timestamp",
    "date_trunc",
    "to_timestamp",
    "to_char",
    "to_date",
    "extract",
    "date_part",
    "age",
    "make_timestamptz",
    // Window ranking.
    "row_number",
    "rank",
    "dense_rank",
    // JSON (read/build only — no execution).
    "json_extract_path_text",
    "jsonb_extract_path_text",
    "json_array_length",
    "jsonb_array_length",
    "json_build_object",
    "jsonb_build_object",
    "json_build_array",
    "jsonb_build_array",
    "to_json",
    "to_jsonb",
    "jsonb_array_elements",
    "jsonb_array_elements_text",
    "jsonb_object_keys",
    "array_agg",
    "string_agg",
    "jsonb_agg",
    "json_agg",
    "unnest",
];

/// Guard plugin raw SQL **on Postgres**, using Postgres's own parser
/// (libpg_query via `pg_query`) rather than a tokenizer, so a reference the
/// tokenizer cannot see as an identifier is still checked. `enforcement` says how
/// the statement is isolated at the database and so how much the parser must add.
///
/// Always enforced, in every mode:
/// - **Relations.** No statement may reference one of HappyView's own internal
///   tables except the allowlisted data ones (`plugin_nameable_relations`);
///   caller-owned tables pass. A `DROP` or `CREATE INDEX` may not target a
///   protected name either.
/// - **Statement kinds.** Read mode permits only a plain `SELECT`; write mode
///   permits DML and — outside `ParserOnly` — `CREATE`/`ALTER`/`DROP` of TABLE,
///   INDEX, VIEW. Temporary objects and `SELECT … INTO` are refused. This also
///   refuses `DO`, `CREATE FUNCTION`, `COPY`, `EXECUTE`, `GRANT`, the role
///   statements, transaction control and — load-bearing — `SET`/`RESET`.
/// - **Forbidden functions.** The [`ALWAYS_FORBIDDEN_FUNCTIONS`] deny-list, which
///   backs the role even in the login pool.
///
/// Enforced outside the login pool (`Role` and `ParserOnly`), where
/// `session_user` is still privileged:
/// - **Function allowlist.** Every function called must be on
///   [`PLUGIN_SQL_FUNCTIONS`]. In `ParserOnly` the relation and function schemas
///   are also restricted to `public`.
///
/// SQL that does not parse, or that cannot be scanned, fails closed. This does
/// not by itself prove a read is side-effect-free; the caller gates read-mode SQL
/// with [`is_read_only`] as well.
pub fn check_postgres_plugin_sql(
    sql: &str,
    mode: PluginSqlMode,
    enforcement: PluginSqlEnforcement,
) -> Result<(), String> {
    use pg_query::protobuf::node::Node;

    let parsed = pg_query::parse(sql).map_err(|e| format!("could not parse SQL: {e}"))?;

    if parsed.protobuf.stmts.is_empty() {
        return Err("raw SQL contained no statement".into());
    }

    // Statement kinds. DDL (caller-owned table management) is allowed unless the
    // parser is the whole boundary; temporary objects and `SELECT … INTO` never.
    let allow_ddl = enforcement.allows_ddl();
    let login_pool = enforcement == PluginSqlEnforcement::LoginPool;
    for raw in &parsed.protobuf.stmts {
        let node = raw.stmt.as_ref().and_then(|n| n.node.as_ref());
        let allowed = match mode {
            PluginSqlMode::Read => {
                matches!(node, Some(Node::SelectStmt(_))) && !statement_creates_temp_or_into(node)
            }
            PluginSqlMode::Write => {
                is_allowed_write_statement(node, allow_ddl)
                    // The login pool additionally lets a plugin define its own
                    // functions, triggers and types in its own schema.
                    || (login_pool && login_pool_object_allowed(node))
            }
        };
        if !allowed {
            return Err(match mode {
                PluginSqlMode::Read => {
                    "database:read permits only read-only SELECT statements in raw SQL".to_string()
                }
                PluginSqlMode::Write => {
                    "raw SQL permits data statements and table DDL, not this statement kind"
                        .to_string()
                }
            });
        }
        if ddl_targets_protected_name(node) {
            return Err(
                "raw SQL cannot drop or index a protected internal HappyView object".to_string(),
            );
        }
        // A plugin may not create or rename an object into HappyView's reserved
        // namespace, even an allowlisted name — another plugin's read of that
        // name would resolve to the plugin's object in the plugin schema.
        if let Some(name) = introduced_object_name(node)
            && is_reserved_object_name(name)
        {
            return Err(format!(
                "raw SQL cannot create or rename an object to the reserved name '{name}'"
            ));
        }
        // A created/altered object must live in the plugin schema, so nothing a
        // plugin defines can be resolved by HappyView's own queries, which never
        // search it.
        if let Some(schema) = ddl_object_schema(node)
            && !schema_is_plugin_or_unqualified(schema)
        {
            return Err(format!(
                "raw SQL may only create objects in the '{PLUGIN_SCHEMA}' schema, not '{schema}'"
            ));
        }
        // A plugin's trigger may attach only to its own tables, never one of
        // HappyView's (including the allowlisted record views).
        if trigger_targets_happyview_relation(node) {
            return Err("raw SQL cannot attach a trigger to a HappyView table".to_string());
        }
    }

    // Every relation and function reference in the whole parse tree. The scan
    // is generic (it walks the serialized tree), so it sees references in a CTE,
    // a scalar subquery, a `RETURNING` list, an `ON CONFLICT` update and a
    // `LATERAL` subquery alike — anywhere a reference can hide. It fails closed
    // if the tree cannot be serialized.
    let (relations, functions) = scan_relations_and_functions(&parsed)?;

    // Relations: refuse HappyView's own internal tables (except the allowlisted
    // data ones). Caller-owned tables pass. In `ParserOnly` a non-`public`
    // relation is refused too, since the privileged connection could otherwise
    // read a catalog or another schema.
    for table in &relations {
        let (schema, name) = split_qualified(table);
        if is_blocked_internal_table(name) {
            return Err(format!(
                "raw SQL cannot reference the protected internal HappyView table '{table}'"
            ));
        }
        if enforcement.restricts_to_public_schema()
            && let Some(schema) = schema
            && !schema.eq_ignore_ascii_case("public")
        {
            return Err(format!(
                "raw SQL cannot reference '{table}' (schema '{schema}' is not allowed here)"
            ));
        }
    }

    // Functions. The deny-list applies in every mode. The allowlist applies
    // outside the login pool, where `session_user` is still privileged.
    let apply_allowlist = enforcement.applies_function_allowlist();
    for func in &functions {
        let (schema, name) = split_qualified(func);
        if ALWAYS_FORBIDDEN_FUNCTIONS
            .iter()
            .any(|f| f.eq_ignore_ascii_case(name))
        {
            return Err(format!("raw SQL cannot call the function '{func}'"));
        }
        if apply_allowlist {
            if let Some(schema) = schema
                && !(schema.eq_ignore_ascii_case("public")
                    || schema.eq_ignore_ascii_case("pg_catalog"))
            {
                return Err(format!(
                    "raw SQL cannot call '{func}' (schema '{schema}' is not allowed)"
                ));
            }
            if !PLUGIN_SQL_FUNCTIONS
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(name))
            {
                return Err(format!(
                    "raw SQL cannot call the function '{func}'; it is not on the plugin SQL allowlist"
                ));
            }
        }
    }

    Ok(())
}

/// Collect every relation and function reference in the parse tree, as
/// `"schema.name"` (or bare `name`). Generic over the whole serialized tree so
/// no reference position is missed; see the caller for why that matters. Fails
/// closed when the tree cannot be serialized, rather than scanning nothing. The
/// parse tree is bounded by the plugin's statement size, so the one allocation
/// is cheap relative to the database round trip it guards.
fn scan_relations_and_functions(
    parsed: &pg_query::ParseResult,
) -> Result<(Vec<String>, Vec<String>), String> {
    let tree = serde_json::to_value(&parsed.protobuf)
        .map_err(|e| format!("could not analyze SQL: {e}"))?;
    let mut relations = Vec::new();
    let mut functions = Vec::new();
    collect_refs(&tree, &mut relations, &mut functions);
    Ok((relations, functions))
}

/// Walk a serialized parse tree, collecting every relation and function
/// reference (both possibly schema-qualified).
///
/// A relation reference is any object carrying a `relname` string: a `RangeVar`
/// appears both tagged inside a `Node` (a `FROM` item) and inline as a typed
/// struct field (`UpdateStmt.relation`, `DeleteStmt.relation`, …), and both
/// shapes have `relname`. A function reference is any object carrying a
/// `funcname` array (a `FuncCall`), whose entries are `String` nodes nested
/// under a `node` wrapper.
fn collect_refs(
    value: &serde_json::Value,
    relations: &mut Vec<String>,
    functions: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(rel) = map.get("relname").and_then(|v| v.as_str()) {
                let schema = map
                    .get("schemaname")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                relations.push(match schema {
                    Some(s) => format!("{s}.{rel}"),
                    None => rel.to_string(),
                });
            }
            if let Some(name) = qualified_name_from_string_nodes(map.get("funcname")) {
                functions.push(name);
            }
            for child in map.values() {
                collect_refs(child, relations, functions);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_refs(child, relations, functions);
            }
        }
        _ => {}
    }
}

/// Turn a `funcname` array of `String` nodes
/// (`[{"node":{"String":{"sval":"lower"}}}]`, or schema-qualified
/// `[{…"pg_catalog"}, {…"set_config"}]`) into `"schema.name"` or bare `name`,
/// keeping the last two parts.
fn qualified_name_from_string_nodes(funcname: Option<&serde_json::Value>) -> Option<String> {
    let parts: Vec<&str> = funcname?
        .as_array()?
        .iter()
        .filter_map(|n| n.get("node")?.get("String")?.get("sval")?.as_str())
        .collect();
    match parts.as_slice() {
        [] => None,
        [name] => Some((*name).to_string()),
        [.., schema, name] => Some(format!("{schema}.{name}")),
    }
}

/// Split a possibly schema-qualified name from `pg_query`'s `tables()` /
/// `functions()` (`"public.happyview_records"`) into `(schema, name)`. Only the
/// last two dot-separated parts matter; a catalog-qualified name keeps its
/// schema so the caller can reject a non-`public` one.
fn split_qualified(qualified: &str) -> (Option<&str>, &str) {
    match qualified.rsplit_once('.') {
        Some((schema, name)) => (Some(schema), name),
        None => (None, qualified),
    }
}

/// Why a raw write to a claimed collection's records is refused.
pub const CLAIMED_WRITE_REFUSED: &str = "writes to records of a collection stored outside this instance must go through save_local in happyview.record";

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
            // `SELECT .. INTO` creates a table; both it and locking clauses write.
            SetExpr::Select(select) => select.into.is_none(),
            SetExpr::Values(_) | SetExpr::Table(_) => true,
            SetExpr::Query(q) => query_is_read_only(q),
            SetExpr::SetOperation { left, right, .. } => {
                set_expr_is_read_only(left) && set_expr_is_read_only(right)
            }
            SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_) => {
                false
            }
        }
    }

    query.locks.is_empty() && set_expr_is_read_only(&query.body)
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
            Statement::Query(q) => query_is_read_only(q) && !has_nested_locks(s),
            _ => false,
        }))
}

/// True when every statement is a write: a statement other than a query, or
/// a query whose body is an `INSERT`, `UPDATE`, `DELETE` or `MERGE`. A read
/// with a data-modifying CTE, a batch mixing reads and writes, and SQL that
/// does not parse are not.
pub fn is_write(sql: &str) -> bool {
    use sqlparser::ast::{SetExpr, Statement};
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;
    let Ok(statements) = Parser::parse_sql(&GenericDialect {}, sql) else {
        return false;
    };
    !statements.is_empty()
        && statements.iter().all(|s| match s {
            Statement::Query(q) => matches!(
                *q.body,
                SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_)
            ),
            _ => true,
        })
}

/// True when any query nested in the statement (a subquery, say) has a
/// locking clause (`FOR UPDATE` / `FOR SHARE`).
fn has_nested_locks(statement: &sqlparser::ast::Statement) -> bool {
    use sqlparser::ast::Visit;

    struct Locks;
    impl sqlparser::ast::Visitor for Locks {
        type Break = ();
        fn pre_visit_query(&mut self, query: &sqlparser::ast::Query) -> std::ops::ControlFlow<()> {
            if query.locks.is_empty() {
                std::ops::ControlFlow::Continue(())
            } else {
                std::ops::ControlFlow::Break(())
            }
        }
    }
    statement.visit(&mut Locks).is_break()
}

/// Why a raw read that mixes record views with other relations is refused.
pub const RAW_MIXED_REFUSED: &str =
    "this query reads claimed records and local tables together; split it into two queries";

/// The compat views that serve record reads.
const RECORD_VIEWS: [&str; 3] = [
    "happyview_records",
    "happyview_record_refs",
    "happyview_records_by_collection",
];

/// True when `table`, an unquoted identifier, names a record view.
pub fn is_record_view(table: &str) -> bool {
    RECORD_VIEWS.contains(&table.to_ascii_lowercase().as_str())
}

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
/// fails closed, and so does anything not plainly a view: a name qualified
/// with a schema other than `main` or `public`, or a name that is not a plain
/// identifier, counts as another relation. So does a `WITH` defining more
/// than [`MAX_CTES`] CTEs, which is refused.
///
/// The check is syntactic, so it cannot see through a user function that reads
/// the views (`my_func()`), or a user-created view built on them. The caller
/// must gate the SQL with [`is_read_only`] first; writes are not classified.
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
    if relations.too_many_ctes {
        return Err(format!(
            "raw SQL that reads record views may define at most {MAX_CTES} CTEs in one WITH"
        ));
    }
    Ok(match relations.other {
        Some(name) => RecordViews::Mixed(name),
        None if relations.saw_view => RecordViews::Only,
        None => RecordViews::None,
    })
}

/// The most CTEs one `WITH` may define in SQL that reads the record views.
/// Each CTE body is checked against the ones before it, so the cost grows
/// with the square of the count; more fails closed.
const MAX_CTES: usize = 1000;

/// Schemas a record view may be qualified with.
const VIEW_SCHEMAS: [&str; 2] = ["main", "public"];

/// Collects the relations a query reads, resolving CTE names by scope.
#[derive(Default)]
struct Relations {
    /// The CTE names visible at the current point, one set per enclosing query.
    scopes: Vec<std::collections::HashSet<String>>,
    saw_view: bool,
    /// The first relation that is not a record view.
    other: Option<String>,
    /// CTE bodies already checked on their own; the main walk skips them.
    checked: std::collections::HashSet<*const sqlparser::ast::Query>,
    /// How many enclosing queries are being skipped.
    skipping: usize,
    /// A `WITH` defined more than [`MAX_CTES`] CTEs; the walk stopped there.
    too_many_ctes: bool,
}

/// An identifier as the database compares it: unquoted names fold to lower case.
fn fold(ident: &sqlparser::ast::Ident) -> String {
    if ident.quote_style.is_some() {
        ident.value.clone()
    } else {
        ident.value.to_ascii_lowercase()
    }
}

impl Relations {
    /// Record a relation read, given its optional schema and its name.
    fn record(&mut self, schema: Option<&str>, name: String) -> std::ops::ControlFlow<()> {
        if self.skipping > 0 {
            return std::ops::ControlFlow::Continue(());
        }
        let schema_ok = schema.is_none_or(|s| VIEW_SCHEMAS.contains(&s));
        // A CTE can only be named unqualified.
        if schema.is_none() && self.scopes.iter().any(|scope| scope.contains(&name)) {
            return std::ops::ControlFlow::Continue(());
        }
        if schema_ok && RECORD_VIEWS.contains(&name.as_str()) {
            self.saw_view = true;
            return std::ops::ControlFlow::Continue(());
        }
        self.other = Some(match schema {
            Some(schema) => format!("{schema}.{name}"),
            None => name,
        });
        std::ops::ControlFlow::Break(())
    }

    fn scan_body(&mut self, body: &sqlparser::ast::SetExpr) -> std::ops::ControlFlow<()> {
        use sqlparser::ast::SetExpr;
        match body {
            // `TABLE x` is a relation read that is not a `TableFactor`.
            SetExpr::Table(table) => {
                let schema = table.schema_name.as_deref().map(str::to_ascii_lowercase);
                match &table.table_name {
                    Some(name) => self.record(schema.as_deref(), name.to_ascii_lowercase()),
                    None => self.record(schema.as_deref(), "table".into()),
                }
            }
            SetExpr::SetOperation { left, right, .. } => {
                self.scan_body(left)?;
                self.scan_body(right)
            }
            _ => std::ops::ControlFlow::Continue(()),
        }
    }
}

impl sqlparser::ast::Visitor for Relations {
    type Break = ();

    fn pre_visit_query(&mut self, query: &sqlparser::ast::Query) -> std::ops::ControlFlow<()> {
        use sqlparser::ast::Visit;

        // A body checked on its own is not walked again, which would make
        // nested CTEs cost exponential time.
        if self.skipping > 0 || self.checked.contains(&(query as *const _)) {
            self.skipping += 1;
            self.scopes.push(Default::default());
            return std::ops::ControlFlow::Continue(());
        }
        let mut scope = std::collections::HashSet::new();
        if let Some(with) = &query.with {
            if with.cte_tables.len() > MAX_CTES {
                self.too_many_ctes = true;
                return std::ops::ControlFlow::Break(());
            }
            // Without RECURSIVE, a CTE body sees only the CTEs before it, so a
            // name it shares with its own CTE or a later one is a real table.
            // The scope below covers every CTE, which is right for the main
            // body but would hide that; check each body on its own.
            if !with.recursive {
                for (i, cte) in with.cte_tables.iter().enumerate() {
                    let mut body = Relations {
                        scopes: self.scopes.clone(),
                        ..Relations::default()
                    };
                    body.scopes.push(
                        with.cte_tables[..i]
                            .iter()
                            .map(|c| fold(&c.alias.name))
                            .collect(),
                    );
                    let _ = cte.query.visit(&mut body);
                    self.checked.insert(&*cte.query as *const _);
                    self.saw_view |= body.saw_view;
                    if body.too_many_ctes {
                        self.too_many_ctes = true;
                        return std::ops::ControlFlow::Break(());
                    }
                    if let Some(name) = body.other {
                        self.other = Some(name);
                        return std::ops::ControlFlow::Break(());
                    }
                }
            }
            scope.extend(with.cte_tables.iter().map(|c| fold(&c.alias.name)));
        }
        self.scopes.push(scope);
        self.scan_body(&query.body)
    }

    fn post_visit_query(&mut self, _query: &sqlparser::ast::Query) -> std::ops::ControlFlow<()> {
        self.scopes.pop();
        self.skipping = self.skipping.saturating_sub(1);
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(
        &mut self,
        factor: &sqlparser::ast::TableFactor,
    ) -> std::ops::ControlFlow<()> {
        use sqlparser::ast::ObjectNamePart;
        // `args: Some(..)` is a table function (`json_each(..)`), not a relation.
        let sqlparser::ast::TableFactor::Table {
            name, args: None, ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let mut idents = Vec::new();
        for part in &name.0 {
            match part {
                ObjectNamePart::Identifier(ident) => idents.push(fold(ident)),
                // Not a plain name: fail closed.
                ObjectNamePart::Function(_) => return self.record(Some("?"), name.to_string()),
            }
        }
        let Some(last) = idents.pop() else {
            return self.record(Some("?"), name.to_string());
        };
        match idents.as_slice() {
            [] => self.record(None, last),
            [schema] => self.record(Some(schema), last),
            _ => self.record(Some(&idents.join(".")), last),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_view_use_stays_fast_on_nested_and_many_ctes() {
        use std::time::Instant;

        // Each level wraps the last in a non-recursive CTE.
        let mut sql = "SELECT * FROM happyview_records".to_string();
        for i in 0..40 {
            sql = format!("WITH c{i} AS ({sql}) SELECT * FROM c{i}");
        }
        let started = Instant::now();
        assert_eq!(record_view_use(&sql), Ok(RecordViews::Only));
        assert!(started.elapsed().as_secs() < 1, "nested CTEs are slow");

        // The same, with a local table at the bottom.
        let mut sql = "SELECT * FROM my_table, happyview_records".to_string();
        for i in 0..40 {
            sql = format!("WITH c{i} AS ({sql}) SELECT * FROM c{i}");
        }
        assert_eq!(
            record_view_use(&sql),
            Ok(RecordViews::Mixed("my_table".into()))
        );

        // Many sibling CTEs, each reading the one before.
        let mut sql = "WITH c0 AS (SELECT * FROM happyview_records)".to_string();
        for i in 1..1000 {
            sql.push_str(&format!(", c{i} AS (SELECT * FROM c{})", i - 1));
        }
        sql.push_str(" SELECT * FROM c999");
        let started = Instant::now();
        assert_eq!(record_view_use(&sql), Ok(RecordViews::Only));
        assert!(started.elapsed().as_secs() < 1, "many CTEs are slow");
    }

    #[test]
    fn record_view_use_scopes_cte_names() {
        use RecordViews::*;
        let cases: &[(&str, &str)] = &[
            // A CTE inside a join subquery does not hide the outer table.
            (
                "SELECT * FROM happyview_records JOIN (WITH my_table AS (SELECT 1 AS uri) SELECT * FROM my_table) s ON true JOIN my_table m ON m.uri = happyview_records.uri",
                "mixed:my_table",
            ),
            // Nor does one in an IN subquery hide the name in a sibling.
            (
                "SELECT * FROM happyview_records WHERE uri IN (WITH notes AS (SELECT 'x' AS uri) SELECT uri FROM notes) AND did IN (SELECT did FROM notes)",
                "mixed:notes",
            ),
            // A non-recursive CTE does not see itself.
            (
                "WITH my_table AS (SELECT * FROM my_table) SELECT * FROM my_table, happyview_records",
                "mixed:my_table",
            ),
            // A CTE that shadows a view reads whatever its body reads.
            (
                "WITH happyview_records AS (SELECT * FROM my_table) SELECT * FROM happyview_records",
                "mixed:my_table",
            ),
            (
                "WITH recursive t AS (SELECT uri FROM happyview_records UNION ALL SELECT uri FROM t) SELECT * FROM t",
                "only",
            ),
            (
                "WITH a AS (SELECT * FROM happyview_records), b AS (SELECT * FROM a) SELECT * FROM b",
                "only",
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
    fn record_view_use_fails_closed_on_hard_shapes() {
        use RecordViews::*;
        let cases: &[(&str, &str)] = &[
            (
                "SELECT uri FROM happyview_records UNION TABLE my_table",
                "mixed:my_table",
            ),
            (
                "SELECT uri FROM happyview_records UNION SELECT uri FROM my_table",
                "mixed:my_table",
            ),
            ("SELECT 1 UNION TABLE happyview_records", "only"),
            (
                "SELECT uri FROM happyview_records UNION TABLE other.my_table",
                "mixed:other.my_table",
            ),
            (
                "SELECT * FROM other.happyview_records",
                "mixed:other.happyview_records",
            ),
            ("SELECT * FROM main.happyview_records", "only"),
            (
                "SELECT * FROM temp.happyview_records",
                "mixed:temp.happyview_records",
            ),
            (
                "SELECT * FROM a.public.happyview_records",
                "mixed:a.public.happyview_records",
            ),
            (
                "SELECT * FROM happyview_records WHERE EXISTS (SELECT 1 FROM my_table)",
                "mixed:my_table",
            ),
            (
                "SELECT * FROM happyview_records WHERE EXISTS (SELECT 1 FROM happyview_record_refs)",
                "only",
            ),
            (
                "SELECT * FROM happyview_records; SELECT * FROM my_table",
                "mixed:my_table",
            ),
            (
                "SELECT * FROM happyview_records; SELECT * FROM happyview_record_refs",
                "only",
            ),
            ("SELECT * FROM \"happyview_records\"", "only"),
            ("SELECT * FROM HappyView_Records", "only"),
            (
                "SELECT * FROM \"HappyView_Records\"",
                "mixed:HappyView_Records",
            ),
            (
                "SELECT * FROM happyview_records, json_each((SELECT data FROM my_table))",
                "mixed:my_table",
            ),
            // A schema-qualified name is never a CTE.
            (
                "WITH my_table AS (SELECT 1) SELECT * FROM happyview_records, public.my_table",
                "mixed:public.my_table",
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
        // SQLite-only syntax is outside the generic dialect: refused, not guessed at.
        assert!(
            record_view_use("SELECT * FROM happyview_records INDEXED BY idx WHERE uri = 'a'")
                .is_err()
        );
    }

    #[test]
    fn read_only_rejects_select_into_and_locking_clauses() {
        assert_eq!(
            is_read_only("SELECT * INTO my_table FROM happyview_records"),
            Ok(false)
        );
        assert_eq!(is_read_only("SELECT * FROM t FOR UPDATE"), Ok(false));
        assert_eq!(is_read_only("SELECT * FROM t FOR SHARE"), Ok(false));
        assert_eq!(
            is_read_only("SELECT * FROM (SELECT * FROM t FOR UPDATE) s"),
            Ok(false)
        );
        assert_eq!(is_read_only("SELECT * FROM t"), Ok(true));
    }

    #[test]
    fn the_by_collection_view_is_a_record_view_raw_sql_may_read() {
        let sql = "SELECT uri, record FROM happyview_records_by_collection \
                   WHERE collection_id = hv_collection_id('app.test.post') \
                   ORDER BY created_at_us DESC, id DESC LIMIT 50";
        assert_eq!(check_raw_sql_tables(sql), Ok(()));
        assert_eq!(record_view_use(sql), Ok(RecordViews::Only));
        // The function reads no relation of the query's own.
        assert_eq!(
            record_view_use(
                "SELECT uri FROM happyview_records_by_collection r \
                 JOIN happyview_jobs j ON j.id = r.uri"
            ),
            Ok(RecordViews::Mixed("happyview_jobs".into()))
        );
        // happyview_collections stays refused: hv_collection_id is the way in.
        assert!(
            check_raw_sql_tables(
                "SELECT id FROM happyview_collections WHERE nsid = 'app.test.post'"
            )
            .is_err()
        );
    }

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
        // Space tables, which `happyview.spaces` reaches with access checks.
        for table in [
            "happyview_spaces",
            "happyview_space_members",
            "happyview_space_records",
            "happyview_space_record_oplog",
            "happyview_space_notify_registrations",
            "happyview_space_dids",
            "happyview_space_credentials",
            "happyview_space_repo_state",
        ] {
            assert!(
                super::check_raw_sql_tables(&format!("SELECT * FROM {table}")).is_err(),
                "{table} should be blocked"
            );
        }
        assert!(
            super::check_raw_sql_tables(
                "INSERT INTO happyview_space_members (id, space_id, member_did) VALUES ($1, $2, $3)"
            )
            .is_err()
        );
        // The migration bookkeeping table is off-limits too.
        assert!(super::check_raw_sql_tables("SELECT * FROM _sqlx_migrations").is_err());
    }

    /// Every space table the migrations create, including any added later, is
    /// out of raw SQL's reach.
    #[tokio::test]
    async fn no_migrated_space_table_is_reachable() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let tables: Vec<(String,)> = crate::db::query_as(
            "SELECT name FROM sqlite_master WHERE type IN ('table', 'view') AND name LIKE 'happyview\\_space%' ESCAPE '\\'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(tables.len() >= 6, "found {tables:?}");
        for (table,) in tables {
            assert!(
                super::check_raw_sql_tables(&format!("SELECT * FROM {table}")).is_err(),
                "{table} is reachable from raw SQL"
            );
        }
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
    fn is_write_tells_writes_from_reads() {
        for sql in [
            "UPDATE happyview_records SET rev = 'x' WHERE collection = 'a.b.c'",
            "DELETE FROM happyview_record_refs WHERE collection = 'a.b.c'",
            "INSERT INTO my_table SELECT uri FROM happyview_records",
            "WITH x AS (SELECT 1) DELETE FROM happyview_records",
            "CREATE TABLE t AS SELECT * FROM happyview_records",
            "UPDATE t SET a = 1; DELETE FROM u",
        ] {
            assert!(is_write(sql), "{sql}");
        }
        for sql in [
            "SELECT * FROM happyview_records",
            "SELECT * FROM happyview_records FOR SHARE",
            "SELECT * INTO t FROM happyview_records",
            // A data-modifying CTE under a read is not a plain write.
            "WITH x AS (DELETE FROM happyview_records RETURNING uri) SELECT * FROM x",
            // Neither is a batch mixing a read with a write.
            "SELECT * FROM happyview_records; DELETE FROM t",
            "SELECT * FROM happyview_records INDEXED BY no_such_index",
            "",
        ] {
            assert!(!is_write(sql), "{sql}");
        }
    }

    #[test]
    fn record_view_use_refuses_too_many_ctes() {
        let ctes = |n: usize| {
            let mut sql = "WITH c0 AS (SELECT * FROM happyview_records)".to_string();
            for i in 1..n {
                sql.push_str(&format!(", c{i} AS (SELECT * FROM c{})", i - 1));
            }
            sql.push_str(&format!(" SELECT * FROM c{}", n - 1));
            sql
        };
        assert_eq!(record_view_use(&ctes(MAX_CTES)), Ok(RecordViews::Only));
        let err = record_view_use(&ctes(MAX_CTES + 1)).unwrap_err();
        assert!(err.contains("CTEs"), "{err}");
        // Nested inside a subquery too.
        let nested = format!("SELECT * FROM ({}) s", ctes(MAX_CTES + 1));
        assert!(record_view_use(&nested).is_err());
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

#[cfg(test)]
mod postgres_plugin_guard_tests {
    //! Parse-level checks for the Postgres plugin-SQL guard. No database: these
    //! assert what the parser accepts and refuses in each enforcement mode. That
    //! the database denies the same statements under the roles is covered by the
    //! integration tests.
    use super::PluginSqlEnforcement::*;
    use super::{PluginSqlEnforcement, PluginSqlMode::*, check_postgres_plugin_sql};

    const ALL: [PluginSqlEnforcement; 3] = [LoginPool, Role, ParserOnly];
    /// The modes that allow caller table DDL (everything but parser-only).
    const DDL_MODES: [PluginSqlEnforcement; 2] = [LoginPool, Role];

    #[test]
    fn allows_plain_reads_of_allowlisted_relations() {
        for e in ALL {
            for sql in [
                "SELECT * FROM happyview_records WHERE collection = $1",
                "SELECT count(*) FROM happyview_records_by_collection",
                "SELECT lower(uri) FROM happyview_record_refs LIMIT 10",
                "SELECT * FROM public.happyview_labels",
            ] {
                assert!(
                    check_postgres_plugin_sql(sql, Read, e).is_ok(),
                    "should allow ({e:?}): {sql}"
                );
            }
        }
    }

    #[test]
    fn allows_caller_table_ddl_in_ddl_modes() {
        for e in DDL_MODES {
            for sql in [
                "CREATE TABLE IF NOT EXISTS my_plugin_state (k TEXT, v TEXT)",
                "INSERT INTO my_plugin_state (k, v) VALUES ($1, $2)",
                "CREATE INDEX my_plugin_state_k ON my_plugin_state (k)",
                "ALTER TABLE my_plugin_state ADD COLUMN n INT",
                "CREATE VIEW my_plugin_view AS SELECT k FROM my_plugin_state",
                "DROP TABLE my_plugin_state",
            ] {
                assert!(
                    check_postgres_plugin_sql(sql, Write, e).is_ok(),
                    "should allow caller DDL ({e:?}): {sql}"
                );
            }
        }
        // DML and reads work in every mode, including parser-only.
        for e in ALL {
            assert!(
                check_postgres_plugin_sql("INSERT INTO my_plugin_state VALUES ($1)", Write, e)
                    .is_ok()
            );
            assert!(check_postgres_plugin_sql("SELECT * FROM my_plugin_state", Read, e).is_ok());
        }
    }

    #[test]
    fn parser_only_refuses_ddl() {
        for sql in [
            "CREATE TABLE my_plugin_state (k TEXT)",
            "CREATE INDEX i ON my_plugin_state (k)",
            "CREATE VIEW v AS SELECT 1",
            "ALTER TABLE my_plugin_state ADD COLUMN n INT",
            "DROP TABLE my_plugin_state",
        ] {
            assert!(
                check_postgres_plugin_sql(sql, Write, ParserOnly).is_err(),
                "parser-only should refuse DDL: {sql}"
            );
        }
    }

    #[test]
    fn executable_objects_refused_outside_the_login_pool() {
        // In role and parser-only modes the session user is privileged, so a
        // plugin may not define functions, triggers or other executable objects.
        for e in [Role, ParserOnly] {
            for sql in [
                "CREATE FUNCTION f() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql",
                "CREATE PROCEDURE p() AS $$ BEGIN END $$ LANGUAGE plpgsql",
                "CREATE TRIGGER t BEFORE INSERT ON my_plugin_state FOR EACH ROW EXECUTE FUNCTION f()",
                "CREATE TYPE my_t AS (a int)",
                "CREATE SEQUENCE my_seq",
            ] {
                assert!(
                    check_postgres_plugin_sql(sql, Write, e).is_err(),
                    "should refuse executable objects ({e:?}): {sql}"
                );
            }
        }
    }

    #[test]
    fn login_pool_allows_plugin_objects_in_its_schema() {
        for sql in [
            "CREATE FUNCTION f() RETURNS int LANGUAGE plpgsql AS $$ BEGIN RETURN 1; END $$",
            "CREATE FUNCTION happyview_plugin.g() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$",
            "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN END $$",
            "CREATE TRIGGER t BEFORE INSERT ON my_plugin_state FOR EACH ROW EXECUTE FUNCTION f()",
            "CREATE TYPE my_t AS (a int)",
            "CREATE SEQUENCE my_seq",
            "CREATE TABLE snapshot AS SELECT * FROM my_plugin_state",
            // A function named like a built-in is fine: it lands in the plugin
            // schema, which HappyView never searches.
            "CREATE FUNCTION lower(varchar) RETURNS text LANGUAGE sql AS $$ SELECT 'x'::text $$",
        ] {
            assert!(
                check_postgres_plugin_sql(sql, Write, LoginPool).is_ok(),
                "login pool should allow a plugin object: {sql}"
            );
        }
    }

    #[test]
    fn login_pool_refuses_security_definer_and_untrusted_language() {
        for sql in [
            "CREATE FUNCTION f() RETURNS int LANGUAGE plpgsql SECURITY DEFINER AS $$ BEGIN RETURN 1; END $$",
            "CREATE FUNCTION f() RETURNS int LANGUAGE c AS 'x', 'y'",
            "CREATE FUNCTION f() RETURNS int LANGUAGE plpython3u AS $$ return 1 $$",
        ] {
            assert!(
                check_postgres_plugin_sql(sql, Write, LoginPool).is_err(),
                "login pool should refuse: {sql}"
            );
        }
    }

    #[test]
    fn login_pool_refuses_altering_a_function_to_security_definer_or_set() {
        for sql in [
            "ALTER FUNCTION f() SECURITY DEFINER",
            "ALTER FUNCTION f() SET search_path = public",
        ] {
            assert!(
                check_postgres_plugin_sql(sql, Write, LoginPool).is_err(),
                "login pool should refuse: {sql}"
            );
        }
        // A benign ALTER (not security/SET) on a plugin-schema function is fine.
        assert!(check_postgres_plugin_sql("ALTER FUNCTION f() COST 10", Write, LoginPool).is_ok());
    }

    #[test]
    fn refuses_creating_or_renaming_to_a_reserved_name() {
        // Even an allowlisted name may not be *created* or renamed to by a plugin.
        for (sql, e) in [
            ("CREATE TABLE happyview_records (a int)", LoginPool),
            ("CREATE VIEW happyview_labels AS SELECT 1", Role),
            (
                "CREATE FUNCTION happyview_evil() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$",
                LoginPool,
            ),
            ("CREATE SEQUENCE happyview_seq", LoginPool),
            ("ALTER TABLE my_t RENAME TO happyview_sneaky", Role),
        ] {
            assert!(
                check_postgres_plugin_sql(sql, Write, e).is_err(),
                "should refuse a reserved name ({e:?}): {sql}"
            );
        }
    }

    #[test]
    fn login_pool_refuses_objects_outside_the_plugin_schema() {
        for sql in [
            "CREATE FUNCTION public.f() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$",
            "CREATE TABLE public.x (a int)",
            "CREATE VIEW public.v AS SELECT 1",
            "CREATE FUNCTION public.lower(varchar) RETURNS text LANGUAGE sql AS $$ SELECT 'x'::text $$",
        ] {
            assert!(
                check_postgres_plugin_sql(sql, Write, LoginPool).is_err(),
                "login pool should refuse a non-plugin-schema object: {sql}"
            );
        }
    }

    #[test]
    fn login_pool_refuses_a_trigger_on_a_happyview_table() {
        for sql in [
            "CREATE TRIGGER t BEFORE INSERT ON happyview_records FOR EACH ROW EXECUTE FUNCTION f()",
            "CREATE TRIGGER t BEFORE INSERT ON happyview_jobs FOR EACH ROW EXECUTE FUNCTION f()",
        ] {
            assert!(
                check_postgres_plugin_sql(sql, Write, LoginPool).is_err(),
                "login pool should refuse a trigger on a HappyView table: {sql}"
            );
        }
    }

    #[test]
    fn always_refused_object_kinds() {
        // These stay refused in every mode, login pool included.
        for e in ALL {
            for sql in [
                "CREATE OPERATOR === (leftarg = int, rightarg = int, function = int4eq)",
                "CREATE CAST (int AS text) WITH FUNCTION f(int)",
                "CREATE AGGREGATE agg(int) (sfunc = int4pl, stype = int)",
                "CREATE RULE r AS ON INSERT TO my_plugin_state DO INSTEAD NOTHING",
                "CREATE EXTENSION hstore",
                "CREATE LANGUAGE plpython3u",
                "CREATE EVENT TRIGGER et ON ddl_command_start EXECUTE FUNCTION f()",
                "DO $$ BEGIN PERFORM 1; END $$",
                "COPY my_table TO '/tmp/x'",
                "SET ROLE postgres",
                "RESET ROLE",
                "SELECT 1; RESET ROLE",
                "BEGIN",
                "GRANT ALL ON my_table TO postgres",
                "ALTER TABLE my_plugin_state OWNER TO postgres",
                "CREATE TEMP TABLE t (a int)",
                "SELECT 1 INTO x",
            ] {
                assert!(
                    check_postgres_plugin_sql(sql, Write, e).is_err(),
                    "should refuse in every mode ({e:?}): {sql}"
                );
            }
        }
    }

    #[test]
    fn deny_list_functions_refused_in_every_mode() {
        // Session-state and SQL-text-executing built-ins are refused even in the
        // login pool, where the function allowlist is otherwise skipped.
        for e in ALL {
            for sql in [
                "SELECT disallowed_fn('x')", // only refused here outside the login pool
                "SELECT set_config('search_path', 'x', false)",
                "SELECT pg_advisory_lock(1)",
                "SELECT lo_import('/etc/passwd')",
                "SELECT pg_read_file('/etc/passwd')",
            ] {
                let res = check_postgres_plugin_sql(sql, Read, e);
                // `disallowed_fn` is allowed in the login pool (allowlist skipped);
                // everything else is on the deny-list and refused everywhere.
                if sql.contains("disallowed_fn") && e == LoginPool {
                    assert!(res.is_ok(), "login pool allows a plain function: {sql}");
                } else {
                    assert!(res.is_err(), "should refuse ({e:?}): {sql}");
                }
            }
        }
    }

    #[test]
    fn refuses_internal_tables_in_every_mode() {
        for e in ALL {
            for sql in [
                "SELECT * FROM happyview_users",
                "SELECT token_hash FROM happyview_api_keys",
                "SELECT * FROM public.happyview_users",
                "SELECT * FROM \"happyview_users\"",
                "WITH x AS (DELETE FROM happyview_users RETURNING id) SELECT * FROM x",
                // The tables behind the record views, in several positions.
                "SELECT * FROM happyview_records_v2",
                "INSERT INTO happyview_record_refs_v2 (uri) VALUES ('x')",
                "UPDATE happyview_dids SET did = did",
                "WITH x AS (DELETE FROM happyview_collections RETURNING id) SELECT * FROM x",
                "SELECT (SELECT count(*) FROM happyview_api_keys) AS n",
                "SELECT * FROM caller_t, LATERAL (SELECT id FROM happyview_users) u",
            ] {
                assert!(
                    check_postgres_plugin_sql(sql, Write, e).is_err(),
                    "should refuse the internal table ({e:?}): {sql}"
                );
            }
        }
    }

    #[test]
    fn refuses_dropping_or_indexing_a_protected_name() {
        for e in ALL {
            for sql in [
                "DROP TABLE happyview_users",
                "DROP TABLE happyview_records_v2",
                "CREATE INDEX happyview_sneaky ON my_plugin_state (k)",
            ] {
                assert!(
                    check_postgres_plugin_sql(sql, Write, e).is_err(),
                    "should refuse ({e:?}): {sql}"
                );
            }
        }
    }

    #[test]
    fn non_allowlisted_functions_refused_outside_the_login_pool() {
        let sql = "SELECT disallowed_fn('x')";
        assert!(check_postgres_plugin_sql(sql, Read, Role).is_err());
        assert!(check_postgres_plugin_sql(sql, Read, ParserOnly).is_err());
        assert!(check_postgres_plugin_sql(sql, Read, LoginPool).is_ok());
    }

    #[test]
    fn parser_only_restricts_relations_to_public() {
        // On the privileged connection a non-public schema must not be read.
        assert!(
            check_postgres_plugin_sql("SELECT * FROM pg_catalog.pg_authid", Read, ParserOnly)
                .is_err()
        );
        // Under a role the database denies it instead, so the parser lets it by.
        for e in DDL_MODES {
            assert!(
                check_postgres_plugin_sql("SELECT * FROM pg_catalog.pg_authid", Read, e).is_ok(),
                "{e:?}"
            );
        }
    }

    #[test]
    fn fallback_function_check_sees_every_dml_position() {
        // In parser-only (DML-only) a non-allowlisted function is caught wherever
        // it hides: a CTE, a scalar subquery, RETURNING, ON CONFLICT, LATERAL.
        for sql in [
            "WITH x AS (SELECT disallowed_fn('x') AS q) SELECT * FROM x",
            "SELECT (SELECT disallowed_fn('x')) AS q",
            "INSERT INTO caller_t (k) VALUES ('a') RETURNING disallowed_fn('x')",
            "INSERT INTO caller_t (k) VALUES ('a') ON CONFLICT (k) DO UPDATE SET v = disallowed_fn('x')",
            "SELECT * FROM caller_t, LATERAL (SELECT disallowed_fn('x')) s",
            "UPDATE caller_t SET v = disallowed_fn('x')",
            "DELETE FROM caller_t WHERE k = disallowed_fn('x')",
        ] {
            assert!(
                check_postgres_plugin_sql(sql, Write, ParserOnly).is_err(),
                "parser-only must refuse a hidden non-allowlisted function: {sql}"
            );
        }
    }

    #[test]
    fn refuses_unparseable_sql() {
        assert!(check_postgres_plugin_sql("SELECT 1 FROM (", Read, Role).is_err());
        assert!(check_postgres_plugin_sql("", Read, Role).is_err());
    }
}
