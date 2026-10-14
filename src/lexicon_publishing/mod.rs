pub mod overview;
pub mod pattern;
pub mod publish;
pub mod status;
pub mod store;
pub mod target;

pub use overview::{LexiconView, Overview, RuleView, overview};
pub use pattern::{Pattern, best_match};
pub use publish::{PublishOutcome, publish, publishable, reconcile, spawn_reconcile, unpublish};
pub use status::{PublishStatus, SCHEMA_COLLECTION, compute_status, record_cid, schema_record};
pub use target::{
    LinkedRepoTargets, TargetInfo, instance_target, linked_repo_target, list_targets,
    target_for_rule,
};

/// The DID named by `_lexicon.<authority>`, or `None` when the name has no
/// TXT record. A resolver is built per call because the shared one panics on
/// an unreadable system DNS configuration, and this is an operator check that
/// must report that rather than take the request down.
pub async fn lookup_lexicon_did(authority: &str) -> Result<Option<String>, String> {
    use hickory_resolver::Resolver;
    use hickory_resolver::proto::rr::RData;

    let name = format!("_lexicon.{authority}.");
    let resolver = Resolver::builder_tokio()
        .map_err(|e| format!("could not read the system DNS configuration: {e}"))?
        .build()
        .map_err(|e| format!("could not build a DNS resolver: {e}"))?;

    let lookup = match resolver.txt_lookup(&name).await {
        Ok(lookup) => lookup,
        Err(e) if e.is_no_records_found() => return Ok(None),
        Err(e) => return Err(format!("DNS lookup for {name} failed: {e}")),
    };

    // A TXT record may be split into several strings; they form one value.
    Ok(lookup
        .answers()
        .iter()
        .filter_map(|r| match &r.data {
            RData::TXT(txt) => Some(
                txt.txt_data
                    .iter()
                    .map(|d| String::from_utf8_lossy(d))
                    .collect::<String>(),
            ),
            _ => None,
        })
        .find_map(|value| value.trim().strip_prefix("did=").map(str::to_string)))
}
