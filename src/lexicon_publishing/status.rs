use serde_json::{Value, json};

pub const SCHEMA_COLLECTION: &str = "com.atproto.lexicon.schema";

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
    Moved,
    Outdated,
    Current,
}

/// `target_did` is `None` when the rule's target cannot be used right now. A
/// rule whose target is gone reports that rather than falling back to a
/// broader rule's target.
pub fn compute_status(
    has_rule: bool,
    target_did: Option<&str>,
    published: Option<(&str, &str)>,
    local_cid: Option<&str>,
) -> PublishStatus {
    if !has_rule {
        return PublishStatus::NoRule;
    }
    let Some(target_did) = target_did else {
        return PublishStatus::TargetUnavailable;
    };
    let Some((published_did, published_cid)) = published else {
        return PublishStatus::Unpublished;
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
            compute_status(false, None, Some(("did:a", "c")), Some("c")),
            NoRule
        );
        assert_eq!(compute_status(false, Some("did:a"), None, None), NoRule);
        assert_eq!(
            compute_status(true, None, Some(("did:a", "c")), Some("c")),
            TargetUnavailable
        );
        assert_eq!(
            compute_status(true, Some("did:a"), None, Some("c")),
            Unpublished
        );
        assert_eq!(
            compute_status(true, Some("did:a"), Some(("did:b", "c")), Some("c")),
            Moved
        );
        assert_eq!(
            compute_status(true, Some("did:a"), Some(("did:a", "old")), Some("c")),
            Outdated
        );
        assert_eq!(
            compute_status(true, Some("did:a"), Some(("did:a", "c")), None),
            Outdated
        );
        assert_eq!(
            compute_status(true, Some("did:a"), Some(("did:a", "c")), Some("c")),
            Current
        );
    }
}
