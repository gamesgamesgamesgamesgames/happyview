//! Forward-time scope checking for service-proxied requests.
//!
//! # Why this is deliberately incomplete
//!
//! There is no general "which scope governs this XRPC method" function to
//! write. The AT Protocol scope grammar answers *semantic* questions — may this
//! collection be created, may this method be called against this audience — and
//! the mapping from a method name to one of those questions lives inside the
//! PDS. Proposal 0011 says outright that it "does not include full definitions
//! for the initial permissions," and `@atproto/oauth-scopes` exposes no such
//! mapping either.
//!
//! So this maps only the handful of operations HappyView can map
//! *unambiguously*, and forwards everything else unchecked for the PDS to
//! enforce. That is honest defence-in-depth rather than a second enforcement
//! layer pretending to be complete — reproducing a guess at the PDS's internals
//! would be permissive where the guess is wrong in one direction and would
//! break working apps in the other.
//!
//! The value is real regardless: repo writes and blob uploads are exactly where
//! a PDS with weak granular-scope enforcement would hurt most.

use happyview_scopes::{RepoAction, ScopePermissions};
use serde_json::Value;

use crate::error::AppError;

/// A permission a forwarded request needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Required {
    /// Every listed action on `collection`.
    Repo {
        collection: String,
        actions: Vec<RepoAction>,
    },
    Blob {
        mime: String,
    },
    Rpc {
        lxm: String,
        aud: String,
    },
}

/// What must be held for this request, or an empty list when the method is one
/// we decline to map.
pub(crate) fn required_for(method: &str, body: &Value, proxy_aud: Option<&str>) -> Vec<Required> {
    // A request being relayed onwards is an `rpc` call to the named audience,
    // whatever the method does at the far end. The header supplies exactly the
    // `aud` the grammar requires, so this case maps cleanly.
    if let Some(aud) = proxy_aud {
        return vec![Required::Rpc {
            lxm: method.to_string(),
            aud: aud.to_string(),
        }];
    }

    let collection = || {
        body.get("collection")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };

    match method {
        "com.atproto.repo.createRecord" => collection()
            .map(|collection| {
                vec![Required::Repo {
                    collection,
                    actions: vec![RepoAction::Create],
                }]
            })
            .unwrap_or_default(),

        "com.atproto.repo.deleteRecord" => collection()
            .map(|collection| {
                vec![Required::Repo {
                    collection,
                    actions: vec![RepoAction::Delete],
                }]
            })
            .unwrap_or_default(),

        // `putRecord` is an upsert, so it needs create *and* update — unless a
        // `swapCid` is supplied, which is a genuine no-create guarantee (the
        // PDS enforces the compare-and-swap) and narrows it to update alone.
        // Same rule `linked_repos::pds::require_put_scope` already applies.
        "com.atproto.repo.putRecord" => collection()
            .map(|collection| {
                let actions = if body.get("swapCid").is_some_and(|v| !v.is_null()) {
                    vec![RepoAction::Update]
                } else {
                    vec![RepoAction::Create, RepoAction::Update]
                };
                vec![Required::Repo {
                    collection,
                    actions,
                }]
            })
            .unwrap_or_default(),

        // One check per write in the batch, each with its own collection and
        // action. A batch is not a single permission.
        "com.atproto.repo.applyWrites" => body
            .get("writes")
            .and_then(|v| v.as_array())
            .map(|writes| writes.iter().filter_map(write_permission).collect())
            .unwrap_or_default(),

        _ => Vec::new(),
    }
}

fn write_permission(write: &Value) -> Option<Required> {
    let collection = write.get("collection")?.as_str()?.to_string();
    let ty = write.get("$type")?.as_str()?;

    let actions = match ty.rsplit_once('#')?.1 {
        "create" => vec![RepoAction::Create],
        "update" => vec![RepoAction::Update],
        "delete" => vec![RepoAction::Delete],
        _ => return None,
    };

    Some(Required::Repo {
        collection,
        actions,
    })
}

/// Refuse the request unless every required permission is held.
pub(crate) fn check(
    granted: &ScopePermissions,
    required: &[Required],
    method: &str,
) -> Result<(), AppError> {
    for requirement in required {
        let (ok, needed) = match requirement {
            Required::Repo {
                collection,
                actions,
            } => (
                actions
                    .iter()
                    .all(|action| granted.allows_repo(collection, *action)),
                actions
                    .iter()
                    .map(|a| format!("repo:{collection}?action={}", a.as_str()))
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            Required::Blob { mime } => (granted.allows_blob(mime), format!("blob:{mime}")),
            Required::Rpc { lxm, aud } => {
                (granted.allows_rpc(lxm, aud), format!("rpc:{lxm}?aud={aud}"))
            }
        };

        if !ok {
            return Err(AppError::Forbidden(format!(
                "this session is not authorized for {method}: it needs {needed}. \
                 Scopes are fixed when a session is created, so this needs a new \
                 authorization with the scope included."
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(collection: &str) -> Value {
        json!({ "repo": "did:plc:x", "collection": collection, "record": {} })
    }

    #[test]
    fn create_record_needs_create_on_its_collection() {
        let required = required_for(
            "com.atproto.repo.createRecord",
            &body("com.example.post"),
            None,
        );
        assert_eq!(
            required,
            vec![Required::Repo {
                collection: "com.example.post".into(),
                actions: vec![RepoAction::Create],
            }]
        );
    }

    #[test]
    fn delete_record_needs_delete() {
        let required = required_for(
            "com.atproto.repo.deleteRecord",
            &body("com.example.post"),
            None,
        );
        assert_eq!(
            required,
            vec![Required::Repo {
                collection: "com.example.post".into(),
                actions: vec![RepoAction::Delete],
            }]
        );
    }

    /// The upsert rule, matching `require_put_scope`: both actions normally,
    /// narrowed to update alone when `swapCid` makes creation impossible.
    #[test]
    fn put_record_needs_both_actions_unless_swap_cid_is_given() {
        let required = required_for(
            "com.atproto.repo.putRecord",
            &body("com.example.post"),
            None,
        );
        assert_eq!(
            required[0],
            Required::Repo {
                collection: "com.example.post".into(),
                actions: vec![RepoAction::Create, RepoAction::Update],
            }
        );

        let mut with_swap = body("com.example.post");
        with_swap["swapCid"] = json!("bafyreiabc");
        let required = required_for("com.atproto.repo.putRecord", &with_swap, None);
        assert_eq!(
            required[0],
            Required::Repo {
                collection: "com.example.post".into(),
                actions: vec![RepoAction::Update],
            }
        );
    }

    /// A null `swapCid` is not a swap — it must not narrow the requirement.
    #[test]
    fn a_null_swap_cid_does_not_narrow_put_record() {
        let mut with_null = body("com.example.post");
        with_null["swapCid"] = Value::Null;
        let required = required_for("com.atproto.repo.putRecord", &with_null, None);
        assert_eq!(
            required[0],
            Required::Repo {
                collection: "com.example.post".into(),
                actions: vec![RepoAction::Create, RepoAction::Update],
            }
        );
    }

    #[test]
    fn apply_writes_checks_every_op_separately() {
        let batch = json!({
            "repo": "did:plc:x",
            "writes": [
                { "$type": "com.atproto.repo.applyWrites#create", "collection": "com.example.a" },
                { "$type": "com.atproto.repo.applyWrites#delete", "collection": "com.example.b" },
            ]
        });
        let required = required_for("com.atproto.repo.applyWrites", &batch, None);
        assert_eq!(
            required,
            vec![
                Required::Repo {
                    collection: "com.example.a".into(),
                    actions: vec![RepoAction::Create]
                },
                Required::Repo {
                    collection: "com.example.b".into(),
                    actions: vec![RepoAction::Delete]
                },
            ]
        );
    }

    #[test]
    fn a_proxied_request_is_an_rpc_call_to_the_named_audience() {
        let required = required_for(
            "com.atproto.repo.createRecord",
            &body("com.example.post"),
            Some("did:web:api.bsky.app#bsky_appview"),
        );
        assert_eq!(
            required,
            vec![Required::Rpc {
                lxm: "com.atproto.repo.createRecord".into(),
                aud: "did:web:api.bsky.app#bsky_appview".into(),
            }]
        );
    }

    /// The deliberate gap: anything not mapped forwards unchecked, because
    /// guessing at the PDS's mapping is worse than deferring to it.
    #[test]
    fn unmapped_methods_require_nothing() {
        assert!(required_for("com.atproto.server.getServiceAuth", &json!({}), None).is_empty());
        assert!(required_for("com.atproto.identity.updateHandle", &json!({}), None).is_empty());
        assert!(required_for("com.example.whatever", &json!({}), None).is_empty());
    }

    /// A malformed body is not a licence to skip the check silently — but it
    /// also cannot be mapped, so it forwards and the PDS rejects it.
    #[test]
    fn a_body_with_no_collection_maps_to_nothing() {
        assert!(required_for("com.atproto.repo.createRecord", &json!({}), None).is_empty());
    }

    #[test]
    fn granted_scopes_satisfy_their_requirement() {
        let granted = ScopePermissions::parse("atproto repo:com.example.post?action=create");
        let required = required_for(
            "com.atproto.repo.createRecord",
            &body("com.example.post"),
            None,
        );
        assert!(check(&granted, &required, "com.atproto.repo.createRecord").is_ok());
    }

    #[test]
    fn a_missing_action_is_refused_and_names_what_is_needed() {
        let granted = ScopePermissions::parse("atproto repo:com.example.post?action=create");
        let required = required_for(
            "com.atproto.repo.putRecord",
            &body("com.example.post"),
            None,
        );
        let err = check(&granted, &required, "com.atproto.repo.putRecord").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("action=update"), "{message}");
    }

    #[test]
    fn transition_generic_satisfies_repo_requirements() {
        let granted = ScopePermissions::parse("atproto transition:generic");
        let required = required_for(
            "com.atproto.repo.createRecord",
            &body("com.example.post"),
            None,
        );
        assert!(check(&granted, &required, "com.atproto.repo.createRecord").is_ok());
    }

    #[test]
    fn a_different_collection_is_refused() {
        let granted = ScopePermissions::parse("atproto repo:com.example.post?action=create");
        let required = required_for(
            "com.atproto.repo.createRecord",
            &body("com.other.post"),
            None,
        );
        assert!(check(&granted, &required, "com.atproto.repo.createRecord").is_err());
    }
}
