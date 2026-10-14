use serde_json::{Value, json};

pub const SCHEMA_COLLECTION: &str = "com.atproto.lexicon.schema";

/// What the attached account is asked for so the instance can publish and
/// unpublish its lexicons. Actions are repeated keys: a comma-joined list is
/// one unknown action, and the authorization server refuses the request.
pub const SCHEMA_WRITE_SCOPE: &str =
    "repo:com.atproto.lexicon.schema?action=create&action=update&action=delete";

/// The lexicon document with `$type` set and nothing else touched, which is
/// what the PDS hashes when it reports the record's CID.
pub fn schema_record(lexicon_json: &Value) -> Value {
    let mut record = lexicon_json.clone();
    if let Some(map) = record.as_object_mut() {
        map.insert("$type".to_string(), json!(SCHEMA_COLLECTION));
    }
    record
}

pub fn record_cid(lexicon_json: &Value) -> Option<String> {
    crate::cid_verify::compute_record_cid(&schema_record(lexicon_json)).map(|c| c.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishStatus {
    NoRule,
    TargetUnavailable,
    Unpublished,
    /// Unpublished by hand, which auto-publish respects.
    Withdrawn,
    Moved,
    Outdated,
    Current,
}

/// `target_did` is `None` when the rule's target cannot be used right now. A
/// rule whose target is gone reports that rather than falling back to a
/// broader rule's target. `withdrawn` is the publication row's mark from an
/// explicit unpublish; it only matters while nothing is published.
pub fn compute_status(
    has_rule: bool,
    target_did: Option<&str>,
    published: Option<(&str, &str)>,
    withdrawn: bool,
    local_cid: Option<&str>,
) -> PublishStatus {
    if !has_rule {
        return PublishStatus::NoRule;
    }
    let Some(target_did) = target_did else {
        return PublishStatus::TargetUnavailable;
    };
    let Some((published_did, published_cid)) = published else {
        return if withdrawn {
            PublishStatus::Withdrawn
        } else {
            PublishStatus::Unpublished
        };
    };
    if published_did != target_did {
        return PublishStatus::Moved;
    }
    if local_cid == Some(published_cid) {
        PublishStatus::Current
    } else {
        PublishStatus::Outdated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dashboard_asks_the_attached_account_for_the_schema_scope() {
        let web = include_str!("../../web/src/lib/attached-account-scope.ts");
        let requested = web
            .split('"')
            .nth(1)
            .expect("ATTACHED_ACCOUNT_SCOPE is a string literal");
        assert_eq!(
            requested,
            format!("atproto identity:* {SCHEMA_WRITE_SCOPE}")
        );
    }

    #[test]
    fn schema_record_replaces_type_and_keeps_the_rest() {
        let doc = json!({"$type": "something.else", "lexicon": 1, "id": "a.b.c", "x-happyview": {"k": 1}});
        let rec = schema_record(&doc);
        assert_eq!(
            rec,
            json!({"$type": SCHEMA_COLLECTION, "lexicon": 1, "id": "a.b.c", "x-happyview": {"k": 1}})
        );
        assert_eq!(doc["$type"], "something.else");
    }

    #[test]
    fn schema_record_adds_type_when_absent() {
        assert_eq!(
            schema_record(&json!({"id": "a.b.c"}))["$type"],
            SCHEMA_COLLECTION
        );
    }

    #[test]
    fn record_cid_matches_cid_verify_and_tracks_content() {
        let doc = json!({"lexicon": 1, "id": "com.example.foo", "description": "one"});
        let expected = crate::cid_verify::compute_record_cid(&schema_record(&doc))
            .unwrap()
            .to_string();
        assert_eq!(record_cid(&doc), Some(expected));
        let changed = json!({"lexicon": 1, "id": "com.example.foo", "description": "two"});
        assert_ne!(record_cid(&doc), record_cid(&changed));
    }

    #[test]
    fn status_branches() {
        use PublishStatus::*;
        assert_eq!(
            compute_status(false, None, Some(("did:a", "c")), false, Some("c")),
            NoRule
        );
        assert_eq!(
            compute_status(false, Some("did:a"), None, false, None),
            NoRule
        );
        assert_eq!(
            compute_status(true, None, Some(("did:a", "c")), false, Some("c")),
            TargetUnavailable
        );
        assert_eq!(
            compute_status(true, Some("did:a"), None, false, Some("c")),
            Unpublished
        );
        assert_eq!(
            compute_status(true, Some("did:a"), Some(("did:b", "c")), false, Some("c")),
            Moved
        );
        assert_eq!(
            compute_status(
                true,
                Some("did:a"),
                Some(("did:a", "old")),
                false,
                Some("c")
            ),
            Outdated
        );
        assert_eq!(
            compute_status(true, Some("did:a"), Some(("did:a", "c")), false, None),
            Outdated
        );
        assert_eq!(
            compute_status(true, Some("did:a"), Some(("did:a", "c")), false, Some("c")),
            Current
        );
    }

    #[test]
    fn withdrawn_applies_only_when_a_rule_and_target_exist_and_nothing_is_published() {
        use PublishStatus::*;
        assert_eq!(
            compute_status(true, Some("did:a"), None, true, Some("c")),
            Withdrawn
        );
        assert_eq!(
            compute_status(false, Some("did:a"), None, true, Some("c")),
            NoRule
        );
        assert_eq!(
            compute_status(true, None, None, true, Some("c")),
            TargetUnavailable
        );
        assert_eq!(
            compute_status(true, Some("did:a"), Some(("did:a", "c")), true, Some("c")),
            Current
        );
        assert_eq!(
            compute_status(true, Some("did:a"), None, false, Some("c")),
            Unpublished
        );
    }
}
