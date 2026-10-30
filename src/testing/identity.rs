//! Identity resolution in a test run: `did:plc:alice` is `alice.test`, and
//! nothing asks PLC or DNS.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::profile::{DidDocument, DidService};

/// The PDS every forged document names. Nothing listens there: a test run's
/// writes go through the in-process PDS, which no URL reaches.
pub const FORGED_PDS: &str = "https://pds.test";

static FORGING: AtomicBool = AtomicBool::new(false);

/// Turn forged resolution on for this process. Called only by the test
/// command, before anything resolves.
///
/// There is deliberately no way to turn it off again: a process that has
/// forged an identity cannot vouch for one afterwards, so it never serves.
pub fn forge_identities() {
    FORGING.store(true, Ordering::SeqCst);
}

pub fn forging() -> bool {
    FORGING.load(Ordering::SeqCst)
}

/// Handles are case-insensitive, so both directions lowercase: otherwise
/// `Alice.test` would name a DID whose document claims `alice.test`, and the
/// two-way check would refuse the pair.
pub fn handle_for(did: &str) -> String {
    if let Some(id) = did.strip_prefix("did:plc:") {
        return format!("{id}.test").to_ascii_lowercase();
    }
    if let Some(host) = did.strip_prefix("did:web:") {
        return host.to_ascii_lowercase();
    }
    did.to_string()
}

pub fn did_for(handle: &str) -> Option<String> {
    handle
        .to_ascii_lowercase()
        .strip_suffix(".test")
        .filter(|id| !id.is_empty() && !id.contains('.'))
        .map(|id| format!("did:plc:{id}"))
}

pub fn did_document(did: &str) -> DidDocument {
    DidDocument {
        also_known_as: vec![format!("at://{}", handle_for(did))],
        verification_method: Vec::new(),
        service: vec![DidService {
            id: "#atproto_pds".into(),
            service_endpoint: FORGED_PDS.into(),
        }],
    }
}

/// Refuse a fetch from the PDS a forged document names, before any request:
/// nothing serves it, and asking would send a DNS query for `pds.test` out of
/// a process that promises not to.
pub fn refuse_forged_pds(endpoint: &str, operation: &str) -> Result<(), String> {
    if forging() && endpoint.trim_end_matches('/') == FORGED_PDS {
        return Err(format!(
            "a test run's accounts have no reachable PDS; {operation} is not served in `happyview test`"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plc_did_is_its_identifier_under_test() {
        assert_eq!(handle_for("did:plc:alice"), "alice.test");
        assert_eq!(did_for("alice.test").as_deref(), Some("did:plc:alice"));
        assert_eq!(handle_for("did:web:example.com"), "example.com");
        assert_eq!(did_for("bob.example"), None);
        assert_eq!(did_for("Alice.test").as_deref(), Some("did:plc:alice"));
        assert_eq!(handle_for("did:plc:Alice"), "alice.test");
    }

    #[test]
    fn a_process_that_never_forges_fetches_from_any_endpoint() {
        assert_eq!(refuse_forged_pds(FORGED_PDS, "anything"), Ok(()));
    }

    #[test]
    fn a_forged_document_names_the_handle_and_the_test_pds() {
        let doc = did_document("did:plc:alice");
        assert_eq!(doc.also_known_as, vec!["at://alice.test".to_string()]);
        assert_eq!(
            crate::profile::find_service_endpoint(&doc, "atproto_pds").as_deref(),
            Some(FORGED_PDS)
        );
    }

    /// Nothing in the library's own tests forges, so this process resolves
    /// the way a serving one does: an unreachable directory is an error, not
    /// a forged answer.
    #[tokio::test]
    async fn a_process_that_never_forges_asks_the_directory() {
        assert!(!forging());
        let http = reqwest::Client::new();
        let unreachable = "http://127.0.0.1:1";
        assert!(
            crate::profile::resolve_did_document(&http, unreachable, "did:plc:alice")
                .await
                .is_err()
        );
        assert!(
            crate::profile::resolve_did_document_once(&http, unreachable, "did:plc:alice")
                .await
                .is_err()
        );
        assert!(
            crate::profile::resolve_profile(&http, unreachable, "did:plc:alice")
                .await
                .is_err()
        );
    }

    /// The switch is process-wide and one-way, so whatever calls it decides
    /// which processes accept forged identities. Only the test command may,
    /// and only integration targets that share their process with no test
    /// that must resolve for real. Calling the runner forges too, so it is
    /// held to the same rule.
    #[test]
    fn only_the_test_command_forges() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let forge = vec![
            root.join("src/testing/runner.rs"),
            root.join("tests/test_runner.rs"),
            root.join("tests/test_runner_identity.rs"),
        ];
        let run = vec![
            root.join("src/run.rs"),
            root.join("src/testing/runner.rs"),
            root.join("tests/test_runner.rs"),
        ];
        // The runner's `run` by its full path, which an aliased import has to
        // spell, and by its module. A grouped import is matched by its members
        // below, so importing `Runner` alone stays allowed.
        let rules = [
            (concat!("forge_", "identities"), &forge),
            (concat!("testing::runner::", "run"), &run),
            (concat!("runner::", "run("), &run),
            (concat!("runner::", "{"), &run),
        ];
        let group = concat!("runner::", "{");
        let names_run = |line: &str| {
            line.split_once(group).is_some_and(|(_, members)| {
                members
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .any(|member| member == "run")
            })
        };
        let signature = concat!("pub fn forge_", "identities() {");
        let definition = root.join("src/testing/identity.rs");
        let fixtures = root.join("tests/fixtures");
        let mut offenders = Vec::new();
        let mut dirs = vec![root.join("src"), root.join("tests")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    // Fixture crates build in place; their output is not ours.
                    let fixture_output = path.ends_with("target")
                        && path.parent().and_then(|p| p.parent()) == Some(&fixtures);
                    if !fixture_output {
                        dirs.push(path);
                    }
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (needle, allowed) in &rules {
                    if allowed.contains(&path) {
                        continue;
                    }
                    for (n, line) in text.lines().enumerate() {
                        let defines = path == definition && line == signature;
                        let grouped = *needle == group && !names_run(line);
                        if line.contains(needle) && !defines && !grouped {
                            offenders.push(format!(
                                "{}:{}",
                                path.strip_prefix(root).unwrap().display(),
                                n + 1
                            ));
                        }
                    }
                }
            }
        }
        offenders.sort();
        offenders.dedup();
        assert!(
            offenders.is_empty(),
            "only the test command turns on forged identities; found:\n{}",
            offenders.join("\n")
        );
    }
}
