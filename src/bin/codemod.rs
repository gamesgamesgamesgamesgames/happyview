//! `happyview-codemod` — runs the v2-to-v3 Lua script rewrite
//! (`happyview::codemod::rewrite`) from a shell or against a live database,
//! rather than only from the admin endpoint. Single-file mode is for
//! previewing the rewrite on a script that isn't stored yet (or checking one
//! in isolation); database mode is for migrating an instance's whole
//! `happyview_scripts` table in one pass, since the admin endpoint only ever
//! touches one row at a time.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use happyview::codemod::{self, ScriptKind};
use happyview::db::{self, DatabaseBackend, adapt_sql, now_rfc3339};

fn print_usage() {
    eprintln!("Usage:");
    eprintln!("  happyview-codemod --file PATH --kind KIND");
    eprintln!("  happyview-codemod --stdin --kind KIND");
    eprintln!("  happyview-codemod --database-url URL [--script ID] [--apply] [--allow-markers]");
    eprintln!();
    eprintln!("KIND is one of: procedure, query, record, label, job");
    eprintln!();
    eprintln!("Rewrites a v2 Lua script onto the v3 handle(input, ctx) contract. A construct");
    eprintln!("with no mechanical rewrite is left in place under a `-- codemod:` comment and");
    eprintln!("reported as a note.");
    eprintln!();
    eprintln!("File and stdin mode print the rewritten source to stdout and notes to");
    eprintln!("stderr. Database mode rewrites stored rows in place (all, or --script ID),");
    eprintln!("printing per-script notes, and writes back only with --apply. A script whose");
    eprintln!("rewrite still has markers is skipped by --apply unless --allow-markers is");
    eprintln!("also given.");
    eprintln!();
    eprintln!("Exit codes: 0 clean, 1 error, 2 if any script has notes (markers).");
}

#[derive(Default)]
struct Args {
    file: Option<PathBuf>,
    stdin: bool,
    kind: Option<String>,
    database_url: Option<String>,
    script: Option<String>,
    apply: bool,
    allow_markers: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut raw = std::env::args().skip(1);
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--file" => {
                let value = raw.next().ok_or("--file requires a path")?;
                args.file = Some(PathBuf::from(value));
            }
            "--stdin" => args.stdin = true,
            "--kind" => {
                let value = raw.next().ok_or("--kind requires a value")?;
                args.kind = Some(value);
            }
            "--database-url" => {
                let value = raw.next().ok_or("--database-url requires a value")?;
                args.database_url = Some(value);
            }
            "--script" => {
                let value = raw.next().ok_or("--script requires an id")?;
                args.script = Some(value);
            }
            "--apply" => args.apply = true,
            "--allow-markers" => args.allow_markers = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}");
            print_usage();
            return ExitCode::from(1);
        }
    };

    if args.database_url.is_some() {
        if args.file.is_some() || args.stdin || args.kind.is_some() {
            eprintln!("error: --database-url cannot be combined with --file, --stdin, or --kind");
            return ExitCode::from(1);
        }
        return run_database_mode(&args).await;
    }

    if args.script.is_some() || args.apply || args.allow_markers {
        eprintln!("error: --script, --apply, and --allow-markers require --database-url");
        return ExitCode::from(1);
    }

    run_source_mode(&args)
}

/// `--file` / `--stdin`: rewrite one script and print the result. Exit code
/// reflects that single script's outcome.
fn run_source_mode(args: &Args) -> ExitCode {
    let use_stdin = match (&args.file, args.stdin) {
        (Some(_), true) => {
            eprintln!("error: pass exactly one of --file or --stdin");
            return ExitCode::from(1);
        }
        (None, false) => {
            eprintln!("error: pass exactly one of --file or --stdin, or --database-url");
            print_usage();
            return ExitCode::from(1);
        }
        (Some(_), false) => false,
        (None, true) => true,
    };

    // Checked before touching stdin: a caller piping input can't repeat a
    // stdin read, so a missing --kind has to fail before it consumes the pipe.
    let kind = match resolve_kind(args) {
        Ok(kind) => kind,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(1);
        }
    };

    let source = if use_stdin {
        let mut buf = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
            eprintln!("error: could not read stdin: {e}");
            return ExitCode::from(1);
        }
        buf
    } else {
        let path = args.file.as_ref().expect("checked above");
        match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(e) => {
                eprintln!("error: could not read {}: {e}", path.display());
                return ExitCode::from(1);
            }
        }
    };

    match codemod::rewrite(&source, kind) {
        Ok(result) => {
            print!("{}", result.source);
            for note in &result.notes {
                eprintln!("line {}: {}", note.line, note.message);
            }
            if result.notes.is_empty() {
                ExitCode::from(0)
            } else {
                ExitCode::from(2)
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn resolve_kind(args: &Args) -> Result<ScriptKind, String> {
    match &args.kind {
        Some(text) => ScriptKind::parse(text).ok_or_else(|| {
            format!("unknown --kind '{text}' (expected procedure, query, record, label, or job)")
        }),
        None => Err("--file and --stdin require --kind".to_string()),
    }
}

/// Connects without `db::connect`'s migrations or SQLite setup — this CLI
/// only ever points at an already-running instance's database, and
/// `db::connect` panics on a bad URL or unreachable host, which would turn
/// the documented "exit 1 on error" into an uncaught panic instead.
async fn connect_cli_pool(url: &str) -> Result<sqlx::AnyPool, sqlx::Error> {
    sqlx::any::install_default_drivers();
    sqlx::any::AnyPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await
}

/// `--database-url`: rewrite every stored Lua script (or just `--script ID`),
/// writing back only with `--apply`. Non-Lua rows are skipped — the contract
/// this rewrites is Lua's.
async fn run_database_mode(args: &Args) -> ExitCode {
    let url = args.database_url.as_deref().expect("checked by caller");
    let backend = DatabaseBackend::from_url(url);
    let pool = match connect_cli_pool(url).await {
        Ok(pool) => pool,
        Err(e) => {
            eprintln!("error: could not connect to database: {e}");
            return ExitCode::from(1);
        }
    };

    let select_sql = if args.script.is_some() {
        adapt_sql(
            "SELECT id, script_type, body FROM happyview_scripts WHERE id = ?",
            backend,
        )
    } else {
        adapt_sql(
            "SELECT id, script_type, body FROM happyview_scripts ORDER BY id",
            backend,
        )
    };
    let mut select = db::query_as::<(String, String, String)>(&select_sql);
    if let Some(id) = &args.script {
        select = select.bind(id);
    }
    let rows = match select.fetch_all(&pool).await {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("error: could not read scripts: {e}");
            return ExitCode::from(1);
        }
    };

    if let Some(id) = &args.script
        && rows.is_empty()
    {
        eprintln!("error: no script with id '{id}'");
        return ExitCode::from(1);
    }

    let mut had_error = false;
    let mut had_markers = false;

    for (id, script_type, body) in rows {
        if script_type != "lua" {
            continue;
        }

        let kind = ScriptKind::from_trigger_id(&id);
        match codemod::rewrite(&body, kind) {
            Ok(result) => {
                let marker_count = result.notes.len();
                if marker_count > 0 {
                    had_markers = true;
                    println!("{id}:");
                    for note in &result.notes {
                        println!("  line {}: {}", note.line, note.message);
                    }
                }

                if args.apply && result.source != body {
                    // Same guard as the endpoint's apply (src/admin/scripts.rs).
                    if marker_count > 0 && !args.allow_markers {
                        println!(
                            "{id}: not applying — {marker_count} marker(s) remain (use --allow-markers to override)"
                        );
                    } else if let Err(e) = write_back(&pool, backend, &id, &result.source).await {
                        had_error = true;
                        eprintln!("{id}: failed to write back: {e}");
                    }
                }
            }
            Err(e) => {
                had_error = true;
                eprintln!("{id}: {e}");
            }
        }
    }

    if had_error {
        ExitCode::from(1)
    } else if had_markers {
        ExitCode::from(2)
    } else {
        ExitCode::from(0)
    }
}

/// Stores a rewritten body, recomputing `outbound_xrpcs` the same way
/// `/admin/scripts` upsert does — the rewrite keeps `xrpc.query`/`xrpc.procedure`
/// call syntax unchanged, but re-deriving rather than assuming that keeps this
/// in lockstep with upsert if that ever stops being true.
async fn write_back(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    id: &str,
    new_body: &str,
) -> Result<(), sqlx::Error> {
    let outbound_xrpcs = happyview::lua_analysis::extract_outbound_xrpcs(new_body);
    let outbound_json = if outbound_xrpcs.is_empty() {
        None
    } else {
        serde_json::to_string(&outbound_xrpcs).ok()
    };

    let sql = adapt_sql(
        "UPDATE happyview_scripts SET body = ?, outbound_xrpcs = ?, updated_at = ? WHERE id = ?",
        backend,
    );
    db::query(&sql)
        .bind(new_body)
        .bind(&outbound_json)
        .bind(now_rfc3339())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
