use happyview::db::{self, DatabaseBackend};
use happyview::spaces::cid_backfill;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut dry_run = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--help" | "-h" => {
                eprintln!("Usage: migrate-space-cids [--dry-run]");
                eprintln!();
                eprintln!("Replaces legacy placeholder space record CIDs with real atproto CIDs,");
                eprintln!("remaps the oplog, and rebuilds each repo's LtHash and commit.");
                eprintln!();
                eprintln!("This normally runs automatically on server startup and does not need");
                eprintln!(
                    "to be invoked by hand. Use it to force a re-scan or to preview changes."
                );
                eprintln!();
                eprintln!("Options:");
                eprintln!("  --dry-run   Compute and report changes, then roll back");
                eprintln!("  -h, --help  Show this help");
                eprintln!();
                eprintln!("Reads DATABASE_URL from the environment.");
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                eprintln!("try --help");
                std::process::exit(2);
            }
        }
    }

    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("DATABASE_URL must be set");
            std::process::exit(2);
        }
    };

    let backend = DatabaseBackend::from_url(&database_url);

    let pool = db::connect(&database_url, backend).await;

    if dry_run {
        println!("Dry run — no changes will be committed.\n");
    }

    match cid_backfill::run(&pool, backend, dry_run).await {
        Ok(report) => {
            println!("Records scanned:        {}", report.records_scanned);
            println!("Records re-CID'd:       {}", report.records_updated);
            println!("Records unencodable:    {}", report.records_unencodable);
            println!("Oplog entries remapped: {}", report.oplog_rows_remapped);
            println!("Oplog entries stale:    {}", report.oplog_rows_unresolved);
            println!("Repos rebuilt:          {}", report.repos_rebuilt);

            if report.records_unencodable > 0 {
                println!(
                    "\nWARNING: {} record(s) could not be encoded as DAG-CBOR and kept their \
                     existing CID. They are not valid atproto records; inspect them directly.",
                    report.records_unencodable
                );
            }
            if report.oplog_rows_unresolved > 0 {
                println!(
                    "\nNOTE: {} oplog entry/entries reference content that no longer exists \
                     (superseded or deleted), so no real CID could be derived. They keep their \
                     placeholder value; this does not affect the current repo hash.",
                    report.oplog_rows_unresolved
                );
            }
            if report.is_noop() {
                println!("\nNothing to repair — all record CIDs are already canonical.");
            } else if dry_run {
                println!("\nDry run complete; rolled back. Re-run without --dry-run to apply.");
            } else {
                println!("\nRepair committed.");
            }
        }
        Err(e) => {
            eprintln!("repair failed (no changes committed): {e}");
            std::process::exit(1);
        }
    }
}
