//! The host's half of TID handling: minting needs a clock and randomness.
//! The codec itself is the SDK's, shared with guests.

use std::time::{SystemTime, UNIX_EPOCH};

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
}
