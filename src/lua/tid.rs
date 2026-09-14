//! The host's half of TID handling: minting needs a clock and randomness,
//! ISO 8601 conversion needs chrono. The codec itself is the SDK's, shared
//! with guests.

use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, TimeZone, Utc};

pub use happyview_plugin_sdk::tid::{
    tid_from_number, tid_from_parts, tid_from_unix_microseconds, tid_to_number,
    tid_to_unix_microseconds,
};

/// A fresh TID for now, with a random 10-bit clock id so two TIDs minted
/// in the same microsecond still differ.
pub fn generate_tid() -> String {
    let us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_micros() as u64;
    let rand_bytes = uuid::Uuid::new_v4();
    let clock_id = u16::from_le_bytes([rand_bytes.as_bytes()[0], rand_bytes.as_bytes()[1]]);
    tid_from_parts(us, clock_id)
}

/// The TID's timestamp as ISO 8601 with microseconds; the clock id is dropped.
pub fn tid_to_iso8601(tid: &str) -> Option<String> {
    let us = tid_to_unix_microseconds(tid)?;
    let dt: DateTime<Utc> = Utc.timestamp_micros(us).single()?;
    Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
}

/// A zero-clock-id TID for an ISO 8601 timestamp.
pub fn tid_from_iso8601(iso: &str) -> Option<String> {
    let dt = iso.parse::<DateTime<Utc>>().ok()?;
    Some(tid_from_unix_microseconds(dt.timestamp_micros()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tid_is_13_chars_from_the_alphabet() {
        let tid = generate_tid();
        assert_eq!(tid.len(), 13, "{tid}");
        for ch in tid.chars() {
            assert!("234567abcdefghijklmnopqrstuvwxyz".contains(ch), "{tid}");
        }
    }

    #[test]
    fn tids_are_unique() {
        assert_ne!(generate_tid(), generate_tid());
    }

    #[test]
    fn tids_are_sortable() {
        let a = generate_tid();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = generate_tid();
        assert!(b > a, "{b} should sort after {a}");
    }

    #[test]
    fn iso8601_round_trips_through_a_tid() {
        let tid = tid_from_iso8601("2025-09-13T15:04:05.000000Z").unwrap();
        assert_eq!(
            tid_to_iso8601(&tid).as_deref(),
            Some("2025-09-13T15:04:05.000000Z")
        );
        assert!(tid_from_iso8601("soon").is_none());
        assert!(tid_to_iso8601("nope").is_none());
    }
}
