//! Walks the vendored interop corpus generated from `@atproto/oauth-scopes`.
//!
//! This is what pins the port. The unit tests inside `src/` describe intent;
//! this file checks that intent against the reference implementation's actual
//! answers. Regenerate with `tests/interop/generate.mjs` — see its header.
//!
//! **Coverage note:** the corpus records each scope's canonical serialisation,
//! but this crate has no formatter and does not assert against it. Nothing in
//! HappyView re-serialises a scope; if that changes, implement `Display` and
//! start checking the `canonical` field, which is already there waiting.

use happyview_scopes::{
    AccountAction, AccountPermission, BlobPermission, IdentityPermission, IncludeScope,
    IncludedPermission, LexPermission, LexPermissionSet, LexValue, RepoAction, RepoPermission,
    RpcPermission, ScopePermissions,
};
use serde_json::Value;

fn corpus() -> Value {
    serde_json::from_str(include_str!("interop/corpus.json")).expect("corpus is valid JSON")
}

fn repo_action(s: &str) -> RepoAction {
    RepoAction::parse(s).unwrap_or_else(|| panic!("corpus has unknown repo action {s}"))
}

fn account_action(s: &str) -> AccountAction {
    AccountAction::parse(s).unwrap_or_else(|| panic!("corpus has unknown account action {s}"))
}

/// Every scope string parses (or fails to) exactly as the reference says.
#[test]
fn parse_verdicts_match_the_reference() {
    let corpus = corpus();
    let cases = corpus["parse"].as_array().expect("parse array");
    assert!(
        cases.len() >= 50,
        "corpus shrank; re-check the vendored file"
    );

    let mut checked = 0;
    for case in cases {
        let scope = case["scope"].as_str().unwrap();
        let prefix = case["prefix"].as_str().unwrap();
        let expected = case["valid"].as_bool().unwrap();

        let actual = match prefix {
            "repo" => RepoPermission::parse(scope).is_some(),
            "rpc" => RpcPermission::parse(scope).is_some(),
            "blob" => BlobPermission::parse(scope).is_some(),
            "identity" => IdentityPermission::parse(scope).is_some(),
            "account" => AccountPermission::parse(scope).is_some(),
            "include" => IncludeScope::parse(scope).is_some(),
            other => panic!("corpus has unhandled prefix {other}"),
        };

        assert_eq!(
            actual, expected,
            "scope {scope:?}: reference says valid={expected}, this crate says {actual}"
        );
        checked += 1;
    }
    assert_eq!(checked, cases.len());
}

/// Every permission question resolves the same way, with transitional scopes
/// honoured — which is how a PDS evaluates a real token.
#[test]
fn match_verdicts_match_the_reference() {
    let corpus = corpus();
    let cases = corpus["matches"].as_array().expect("matches array");
    assert!(
        cases.len() >= 250,
        "corpus shrank; re-check the vendored file"
    );

    for case in cases {
        let grant = case["grant"].as_str().unwrap();
        let expected = case["allowed"].as_bool().unwrap();
        let perms = ScopePermissions::parse(grant);

        let (actual, label) = match case["kind"].as_str().unwrap() {
            "repo" => {
                let collection = case["collection"].as_str().unwrap();
                let action = case["action"].as_str().unwrap();
                (
                    perms.allows_repo(collection, repo_action(action)),
                    format!("repo {collection} {action}"),
                )
            }
            "rpc" => {
                let lxm = case["lxm"].as_str().unwrap();
                let aud = case["aud"].as_str().unwrap();
                (perms.allows_rpc(lxm, aud), format!("rpc {lxm} @ {aud}"))
            }
            "blob" => {
                let mime = case["mime"].as_str().unwrap();
                (perms.allows_blob(mime), format!("blob {mime}"))
            }
            "identity" => {
                let attr = case["attr"].as_str().unwrap();
                (perms.allows_identity(attr), format!("identity {attr}"))
            }
            "account" => {
                let attr = case["attr"].as_str().unwrap();
                let action = case["action"].as_str().unwrap();
                (
                    perms.allows_account(attr, account_action(action)),
                    format!("account {attr} {action}"),
                )
            }
            other => panic!("corpus has unhandled match kind {other}"),
        };

        assert_eq!(
            actual, expected,
            "grant {grant:?} asked {label}: reference says {expected}, this crate says {actual}"
        );
    }
}

/// `include:` expansion agrees with the reference, including the
/// authority-containment drops and the `inheritAud` rules.
///
/// The reference returns canonical scope strings and this crate returns typed
/// permissions, so the comparison runs through this crate's own parser: each
/// reference string is parsed and compared structurally. That is sound because
/// `parse_verdicts_match_the_reference` already pins the parser itself.
#[test]
fn include_expansion_matches_the_reference() {
    let corpus = corpus();
    let cases = corpus["includes"].as_array().expect("includes array");
    assert!(!cases.is_empty());

    for case in cases {
        let scope = case["scope"].as_str().unwrap();
        let include = IncludeScope::parse(scope).expect("corpus include scope should parse");
        let set = lex_set(&case["set"]);

        let expected: Vec<&str> = case["expanded"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();

        let actual = include.expand(&set);

        assert_eq!(
            actual.len(),
            expected.len(),
            "{scope}: reference expanded to {expected:?}, this crate produced {actual:?}"
        );

        for (got, want_str) in actual.iter().zip(expected.iter()) {
            let want = parse_included(want_str)
                .unwrap_or_else(|| panic!("could not parse reference output {want_str:?}"));
            assert_eq!(
                got, &want,
                "{scope}: expected {want_str:?}, this crate produced {got:?}"
            );
        }
    }
}

fn parse_included(scope: &str) -> Option<IncludedPermission> {
    if let Some(p) = RepoPermission::parse(scope) {
        return Some(IncludedPermission::Repo(p));
    }
    RpcPermission::parse(scope).map(IncludedPermission::Rpc)
}

/// Convert a corpus permission-set document into the crate's types, preserving
/// scalar/list arity — which the reference treats as significant.
fn lex_set(value: &Value) -> LexPermissionSet {
    let permissions = value["permissions"]
        .as_array()
        .expect("permissions array")
        .iter()
        .map(|p| {
            let obj = p.as_object().expect("permission object");
            let resource = obj["resource"].as_str().expect("resource").to_string();
            let params = obj
                .iter()
                .filter(|(k, _)| k.as_str() != "resource" && k.as_str() != "type")
                .map(|(k, v)| {
                    let value = match v {
                        Value::Array(items) => LexValue::List(
                            items
                                .iter()
                                .map(|i| i.as_str().expect("string item").to_string())
                                .collect(),
                        ),
                        Value::Bool(b) => LexValue::Bool(*b),
                        Value::String(s) => LexValue::Scalar(s.clone()),
                        other => panic!("unsupported corpus param value {other:?}"),
                    };
                    (k.clone(), value)
                })
                .collect();
            LexPermission { resource, params }
        })
        .collect();

    LexPermissionSet { permissions }
}
