//! The telemetry wire format.

use std::collections::BTreeMap;

pub const SCHEMA_VERSION: u32 = 1;

pub type Counters = BTreeMap<String, i64>;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LexiconReport {
    pub count: u32,
    pub top_collection_shares: Vec<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structures: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub instance_id: String,
    pub reported_at: String,
    pub report_mode: String,
    pub happyview_version: String,
    pub process_started_at: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,

    #[serde(default)]
    pub totals: Counters,
    #[serde(default)]
    pub since_start: Counters,
    #[serde(default)]
    pub features: BTreeMap<String, FeatureUsage>,
    #[serde(default)]
    pub host: BTreeMap<String, serde_json::Value>,

    pub lexicons: LexiconReport,
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct FeatureUsage {
    pub ever: bool,
    pub active: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Snapshot {
        Snapshot {
            schema_version: SCHEMA_VERSION,
            instance_id: "11111111-2222-3333-4444-555555555555".into(),
            reported_at: "2026-08-18T12:00:00+00:00".into(),
            report_mode: "auto".into(),
            happyview_version: "0.1.0".into(),
            process_started_at: "2026-08-18T00:00:00+00:00".into(),
            contact: None,
            totals: Default::default(),
            since_start: Default::default(),
            features: Default::default(),
            host: Default::default(),
            lexicons: LexiconReport::default(),
        }
    }

    #[test]
    fn omits_contact_entirely_when_not_supplied() {
        let json = serde_json::to_value(sample()).unwrap();
        assert!(
            !json.as_object().unwrap().contains_key("contact"),
            "a refused toggle must be absent, not null"
        );
    }

    #[test]
    fn includes_contact_when_supplied() {
        let mut s = sample();
        s.contact = Some("tre@trezy.com".into());
        let json = serde_json::to_value(s).unwrap();
        assert_eq!(json["contact"], "tre@trezy.com");
    }

    #[test]
    fn lexicon_names_absent_unless_consented() {
        let json = serde_json::to_value(sample()).unwrap();
        let lex = json["lexicons"].as_object().unwrap();
        assert!(lex.contains_key("count"), "shape data is always present");
        assert!(!lex.contains_key("names"));
        assert!(!lex.contains_key("structures"));
        assert!(!lex.contains_key("documents"));
    }

    #[test]
    fn lexicon_toggles_serialize_independently() {
        let mut s = sample();
        s.lexicons.structures = Some(vec![serde_json::json!({"defs": 2})]);
        let json = serde_json::to_value(s).unwrap();
        let lex = json["lexicons"].as_object().unwrap();

        assert!(lex.contains_key("structures"));
        assert!(
            !lex.contains_key("names"),
            "structure must not drag names along"
        );
        assert!(!lex.contains_key("documents"));
    }

    #[test]
    fn totals_and_since_start_are_separate_objects() {
        let mut s = sample();
        s.totals.insert("records".into(), 42);
        s.since_start.insert("jetstream_events_received".into(), 7);
        let json = serde_json::to_value(s).unwrap();

        assert_eq!(json["totals"]["records"], 42);
        assert_eq!(json["since_start"]["jetstream_events_received"], 7);
        assert!(json["totals"].get("jetstream_events_received").is_none());
    }

    #[test]
    fn unknown_fields_survive_a_round_trip_through_the_maps() {
        let mut s = sample();
        s.totals.insert("a_counter_added_next_year".into(), 1);
        let encoded = serde_json::to_string(&s).unwrap();
        let decoded: Snapshot = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.totals.get("a_counter_added_next_year"), Some(&1));
    }
}
