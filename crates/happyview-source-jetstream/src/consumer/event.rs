//! Jetstream frames, v2 (`subscribeEvents`) and v1 (`/subscribe`), parsed
//! into one event shape.

use serde::Deserialize;
use serde_json::Value;

/// Where an event sits in the stream, in the coordinate its protocol resumes
/// from: a v2 seq (with the witnessed time, for a resume on another host), or
/// a v1 `time_us`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorPoint {
    V2 { seq: i64, witnessed_us: i64 },
    V1 { time_us: i64 },
}

impl CursorPoint {
    pub fn seq(&self) -> Option<i64> {
        match self {
            CursorPoint::V2 { seq, .. } => Some(*seq),
            CursorPoint::V1 { .. } => None,
        }
    }

    /// When Jetstream witnessed the event, in unix microseconds.
    pub fn time_us(&self) -> i64 {
        match self {
            CursorPoint::V2 { witnessed_us, .. } => *witnessed_us,
            CursorPoint::V1 { time_us } => *time_us,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub did: String,
    pub cursor: CursorPoint,
    pub kind: EventKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EventKind {
    Commit(Commit),
    Identity {
        handle: Option<String>,
    },
    Account {
        active: bool,
        status: Option<String>,
    },
    Sync {
        rev: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Commit {
    #[serde(default)]
    pub rev: Option<String>,
    pub operation: String,
    pub collection: String,
    pub rkey: String,
    #[serde(default)]
    pub record: Option<Value>,
    #[serde(default)]
    pub cid: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Frame {
    Event(Event),
    /// `#info`: no seq, never moves the cursor.
    Info {
        name: String,
        message: Option<String>,
    },
    /// A terminal error frame; the server closes the connection after it.
    Error {
        error: String,
        message: Option<String>,
    },
    /// A type this version does not know, skipped for forward compatibility.
    Skip,
}

const V2_KIND_PREFIX: &str = "network.bsky.jetstream.subscribeEvents#";

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "$type")]
    kind: String,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct V2Common {
    seq: i64,
    did: String,
    time: String,
    #[serde(default)]
    witnessed_at: Option<String>,
}

#[derive(Deserialize)]
struct AccountBody {
    active: bool,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize)]
struct IdentityBody {
    #[serde(default)]
    handle: Option<String>,
}

#[derive(Deserialize)]
struct SyncBody {
    #[serde(default)]
    rev: Option<String>,
}

#[derive(Deserialize)]
struct V2Account {
    account: AccountBody,
}

#[derive(Deserialize)]
struct V2Identity {
    identity: IdentityBody,
}

#[derive(Deserialize)]
struct V2Sync {
    sync: SyncBody,
}

#[derive(Deserialize)]
struct V2Info {
    name: String,
    #[serde(default)]
    message: Option<String>,
}

/// RFC 3339 (Jetstream sends six fractional digits) to unix microseconds.
pub(crate) fn parse_time_us(s: &str) -> Result<i64, String> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|t| t.timestamp_micros())
        .map_err(|e| format!("bad timestamp {s:?}: {e}"))
}

/// One v2 text frame.
pub fn parse_v2(text: &str) -> Result<Frame, String> {
    let envelope: Envelope = serde_json::from_str(text).map_err(|e| e.to_string())?;
    match envelope.kind.as_str() {
        "message" => {}
        "error" => {
            return Ok(Frame::Error {
                error: envelope.error.unwrap_or_default(),
                message: envelope.message,
            });
        }
        _ => return Ok(Frame::Skip),
    }
    let payload = envelope
        .payload
        .ok_or_else(|| "message frame without a payload".to_string())?;
    let payload_type = payload
        .get("$type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(kind) = payload_type.strip_prefix(V2_KIND_PREFIX) else {
        return Ok(Frame::Skip);
    };
    let kind = kind.to_string();
    let bad = |e: serde_json::Error| format!("bad #{kind} payload: {e}");

    if kind == "info" {
        let info: V2Info = serde_json::from_value(payload).map_err(bad)?;
        return Ok(Frame::Info {
            name: info.name,
            message: info.message,
        });
    }
    if !matches!(kind.as_str(), "commit" | "identity" | "account" | "sync") {
        return Ok(Frame::Skip);
    }

    let common = V2Common::deserialize(&payload).map_err(bad)?;
    let witnessed = common.witnessed_at.as_deref().unwrap_or(&common.time);
    let cursor = CursorPoint::V2 {
        seq: common.seq,
        witnessed_us: parse_time_us(witnessed)?,
    };
    let event_kind = match kind.as_str() {
        "commit" => EventKind::Commit(serde_json::from_value(payload).map_err(bad)?),
        "account" => {
            let body: V2Account = serde_json::from_value(payload).map_err(bad)?;
            EventKind::Account {
                active: body.account.active,
                status: body.account.status,
            }
        }
        "identity" => {
            let body: V2Identity = serde_json::from_value(payload).map_err(bad)?;
            EventKind::Identity {
                handle: body.identity.handle,
            }
        }
        _ => {
            let body: V2Sync = serde_json::from_value(payload).map_err(bad)?;
            EventKind::Sync { rev: body.sync.rev }
        }
    };
    Ok(Frame::Event(Event {
        did: common.did,
        cursor,
        kind: event_kind,
    }))
}

#[derive(Deserialize)]
struct V1Frame {
    did: String,
    time_us: i64,
    kind: String,
    #[serde(default)]
    commit: Option<Commit>,
    #[serde(default)]
    identity: Option<IdentityBody>,
    #[serde(default)]
    account: Option<AccountBody>,
}

/// One v1 frame. v1 has no seq HappyView can rely on (an original v1 server
/// sends none, and `time_us` is neither unique nor monotonic), so the cursor
/// is the `time_us` and nothing is deduplicated by it.
pub(crate) fn parse_v1(text: &str) -> Result<Frame, String> {
    let frame: V1Frame = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let kind = match (
        frame.kind.as_str(),
        frame.commit,
        frame.identity,
        frame.account,
    ) {
        ("commit", Some(commit), _, _) => EventKind::Commit(commit),
        ("identity", _, Some(identity), _) => EventKind::Identity {
            handle: identity.handle,
        },
        ("account", _, _, Some(account)) => EventKind::Account {
            active: account.active,
            status: account.status,
        },
        _ => return Ok(Frame::Skip),
    };
    Ok(Frame::Event(Event {
        did: frame.did,
        cursor: CursorPoint::V1 {
            time_us: frame.time_us,
        },
        kind,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITNESSED_US: i64 = 1_779_719_009_950_123;
    const TIME_US: i64 = 1_779_719_009_950_000;

    fn message(payload: &str) -> String {
        format!(r#"{{"$type":"message","payload":{payload}}}"#)
    }

    fn event(text: &str, parse: fn(&str) -> Result<Frame, String>) -> Event {
        match parse(text).expect("parses") {
            Frame::Event(event) => event,
            other => panic!("expected an event, got {other:?}"),
        }
    }

    #[test]
    fn a_v2_commit_carries_seq_witnessed_time_and_rev() {
        let text = message(
            r#"{"$type":"network.bsky.jetstream.subscribeEvents#commit","seq":42,"did":"did:plc:abc",
                "time":"2026-05-25T14:23:29.950000Z","witnessedAt":"2026-05-25T14:23:29.950123Z",
                "rev":"3mmoojp7vgo2g","operation":"create","collection":"app.test.post","rkey":"3mmoojp7etg2g",
                "record":{"$type":"app.test.post","text":"hi"},"cid":"bafyreiabc"}"#,
        );
        assert_eq!(
            event(&text, parse_v2),
            Event {
                did: "did:plc:abc".into(),
                cursor: CursorPoint::V2 {
                    seq: 42,
                    witnessed_us: WITNESSED_US
                },
                kind: EventKind::Commit(Commit {
                    rev: Some("3mmoojp7vgo2g".into()),
                    operation: "create".into(),
                    collection: "app.test.post".into(),
                    rkey: "3mmoojp7etg2g".into(),
                    record: Some(serde_json::json!({"$type": "app.test.post", "text": "hi"})),
                    cid: Some("bafyreiabc".into()),
                }),
            }
        );
    }

    #[test]
    fn a_v2_event_without_witnessed_at_uses_its_time() {
        let text = message(
            r#"{"$type":"network.bsky.jetstream.subscribeEvents#commit","seq":7,"did":"did:plc:abc",
                "time":"2026-05-25T14:23:29.950000Z","rev":"3mmoojp7vgo2g","operation":"delete",
                "collection":"app.test.post","rkey":"r"}"#,
        );
        let e = event(&text, parse_v2);
        assert_eq!(
            e.cursor,
            CursorPoint::V2 {
                seq: 7,
                witnessed_us: TIME_US
            }
        );
        let EventKind::Commit(commit) = e.kind else {
            panic!("commit")
        };
        assert_eq!(commit.record, None);
        assert_eq!(commit.cid, None);
    }

    #[test]
    fn v2_account_identity_and_sync_events_parse() {
        let account = message(
            r#"{"$type":"network.bsky.jetstream.subscribeEvents#account","seq":43,"did":"did:plc:abc",
                "time":"2026-05-25T14:23:29.950000Z","witnessedAt":"2026-05-25T14:23:29.950123Z",
                "account":{"seq":9,"did":"did:plc:abc","time":"2026-05-25T14:23:29.950Z","active":false,"status":"deleted"}}"#,
        );
        assert_eq!(
            event(&account, parse_v2).kind,
            EventKind::Account {
                active: false,
                status: Some("deleted".into())
            }
        );
        let identity = message(
            r#"{"$type":"network.bsky.jetstream.subscribeEvents#identity","seq":44,"did":"did:plc:abc",
                "time":"2026-05-25T14:23:29.950000Z","identity":{"seq":10,"did":"did:plc:abc",
                "time":"2026-05-25T14:23:29.950Z","handle":"alice.example.com"}}"#,
        );
        assert_eq!(
            event(&identity, parse_v2).kind,
            EventKind::Identity {
                handle: Some("alice.example.com".into())
            }
        );
        let sync = message(
            r#"{"$type":"network.bsky.jetstream.subscribeEvents#sync","seq":45,"did":"did:plc:abc",
                "time":"2026-05-25T14:23:29.950000Z","sync":{"seq":11,"did":"did:plc:abc",
                "blocks":{"$bytes":""},"rev":"3mmoojp7vgo2g","time":"2026-05-25T14:23:29.950Z"}}"#,
        );
        assert_eq!(
            event(&sync, parse_v2).kind,
            EventKind::Sync {
                rev: Some("3mmoojp7vgo2g".into())
            }
        );
    }

    #[test]
    fn v2_info_and_error_frames_carry_no_cursor() {
        let info = message(
            r#"{"$type":"network.bsky.jetstream.subscribeEvents#info","name":"OutdatedCursor","message":"resumed at seq 90"}"#,
        );
        assert_eq!(
            parse_v2(&info).unwrap(),
            Frame::Info {
                name: "OutdatedCursor".into(),
                message: Some("resumed at seq 90".into())
            }
        );
        assert_eq!(
            parse_v2(r#"{"$type":"error","error":"ConsumerTooSlow","message":"too slow"}"#)
                .unwrap(),
            Frame::Error {
                error: "ConsumerTooSlow".into(),
                message: Some("too slow".into())
            }
        );
    }

    #[test]
    fn unknown_v2_types_are_skipped_and_garbage_is_an_error() {
        assert_eq!(
            parse_v2(r#"{"$type":"future","x":1}"#).unwrap(),
            Frame::Skip
        );
        assert_eq!(
            parse_v2(&message(
                r#"{"$type":"network.bsky.jetstream.subscribeEvents#future","seq":1}"#
            ))
            .unwrap(),
            Frame::Skip
        );
        assert_eq!(
            parse_v2(&message(r#"{"$type":"com.example.other#commit","seq":1}"#)).unwrap(),
            Frame::Skip
        );
        assert!(parse_v2("not json").is_err());
    }

    // Lines from bluesky-social/jetstream internal/subscribe/testdata/golden_v1.jsonl.
    const V1_CREATE: &str = r#"{"did":"did:plc:mcc5yghbk7tixqryac2yxku7","time_us":1779719009132431,"kind":"commit","commit":{"rev":"3mmoojp7vgo2g","operation":"create","collection":"app.bsky.feed.like","rkey":"3mmoojp7etg2g","record":{"$type":"app.bsky.feed.like","createdAt":"2026-05-25T14:23:28.778Z","subject":{"cid":"bafyreiaxf7tgbfydx3465sckitq6v564thxe2wwydbi7cwxbxvgssje3em","uri":"at://did:plc:vfnmgxg2yscfliemmcxzyfyh/app.bsky.feed.post/3mmoodizfwc2j"}},"cid":"bafyreidqpc7si3dbimffi64lz3xsx2qt45ip7rpna2qpslcddp23itjjzm"}}"#;
    const V1_DELETE: &str = r#"{"did":"did:plc:3xnadku4kemqnycbweqnvgzx","time_us":1779719009153272,"kind":"commit","commit":{"rev":"3mmoojpae7c2e","operation":"delete","collection":"org.hypercerts.claim.activity","rkey":"3mmontn3gnc2e"}}"#;
    const V1_ACCOUNT: &str = r#"{"did":"did:plc:g4ut2w5755qxi3fe7mfs72p4","time_us":1779719010272410,"kind":"account","account":{"active":true,"did":"did:plc:g4ut2w5755qxi3fe7mfs72p4","seq":30340180351,"time":"2026-05-25T14:23:29.950Z"}}"#;
    const V1_IDENTITY: &str = r#"{"did":"did:plc:g4ut2w5755qxi3fe7mfs72p4","time_us":1779719010267528,"kind":"identity","identity":{"did":"did:plc:g4ut2w5755qxi3fe7mfs72p4","seq":30340180350,"time":"2026-05-25T14:23:29.950Z"}}"#;

    #[test]
    fn v1_frames_parse_with_a_time_cursor() {
        let create = event(V1_CREATE, parse_v1);
        assert_eq!(
            create.cursor,
            CursorPoint::V1 {
                time_us: 1_779_719_009_132_431
            }
        );
        assert_eq!(create.cursor.seq(), None, "v1 has no seq to deduplicate on");
        let EventKind::Commit(commit) = create.kind else {
            panic!("commit")
        };
        assert_eq!(commit.rev.as_deref(), Some("3mmoojp7vgo2g"));
        assert!(commit.record.is_some());

        let EventKind::Commit(delete) = event(V1_DELETE, parse_v1).kind else {
            panic!("commit")
        };
        assert_eq!((delete.operation.as_str(), delete.record), ("delete", None));

        assert_eq!(
            event(V1_ACCOUNT, parse_v1).kind,
            EventKind::Account {
                active: true,
                status: None
            }
        );
        assert_eq!(
            event(V1_IDENTITY, parse_v1).kind,
            EventKind::Identity { handle: None }
        );
        assert_eq!(
            parse_v1(r#"{"did":"did:plc:x","time_us":1779719010267528,"kind":"future"}"#).unwrap(),
            Frame::Skip
        );
    }

    #[test]
    fn a_cursor_point_exposes_its_seq_and_time() {
        let v2 = CursorPoint::V2 {
            seq: 5,
            witnessed_us: 99,
        };
        assert_eq!((v2.seq(), v2.time_us()), (Some(5), 99));
        let v1 = CursorPoint::V1 { time_us: 77 };
        assert_eq!((v1.seq(), v1.time_us()), (None, 77));
    }
}
