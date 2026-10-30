//! The AT Protocol TID codec: a 64-bit value of `(microseconds << 10) |
//! clock_id` in 13 characters of base32-sortstring. Pure, so a guest can
//! mint record keys from a timestamp the host hands it; drawing the
//! timestamp and the clock id is the caller's job because a guest has no
//! clock and no randomness.

use alloc::string::String;
use alloc::vec::Vec;

const BASE32_SORT: &[u8; 32] = b"234567abcdefghijklmnopqrstuvwxyz";

/// A TID from its two parts. `clock_id` is masked to its 10 bits.
pub fn tid_from_parts(unix_micros: u64, clock_id: u16) -> String {
    encode_base32_sort((unix_micros << 10) | u64::from(clock_id & 0x3FF))
}

/// A TID with a zero clock id: sorts correctly against TIDs from the same
/// moment without matching any specific generated one.
pub fn tid_from_unix_microseconds(us: i64) -> String {
    encode_base32_sort((us as u64) << 10)
}

/// The microsecond timestamp in a TID; the clock id is dropped.
pub fn tid_to_unix_microseconds(tid: &str) -> Option<i64> {
    decode_base32_sort(tid).map(|val| (val >> 10) as i64)
}

/// The full 64-bit value, timestamp and clock id together.
pub fn tid_to_number(tid: &str) -> Option<u64> {
    decode_base32_sort(tid)
}

pub fn tid_from_number(val: u64) -> String {
    encode_base32_sort(val)
}

fn encode_base32_sort(mut val: u64) -> String {
    let mut buf = [0u8; 13];
    for slot in buf.iter_mut().rev() {
        *slot = BASE32_SORT[(val & 0x1F) as usize];
        val >>= 5;
    }
    // Every byte came from BASE32_SORT, which is ASCII.
    String::from_utf8(Vec::from(buf)).expect("base32-sort output is ASCII")
}

fn decode_base32_sort(tid: &str) -> Option<u64> {
    if tid.len() != 13 {
        return None;
    }
    let mut val: u64 = 0;
    for byte in tid.bytes() {
        let idx = BASE32_SORT.iter().position(|&b| b == byte)?;
        val = (val << 5) | idx as u64;
    }
    Some(val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_encodes_to_the_first_alphabet_character() {
        assert_eq!(encode_base32_sort(0), "2222222222222");
    }

    #[test]
    fn decode_inverts_encode() {
        let val: u64 = 0x123456789ABCDEF;
        assert_eq!(decode_base32_sort(&encode_base32_sort(val)), Some(val));
        assert_eq!(tid_to_number(&tid_from_number(val)), Some(val));
    }

    #[test]
    fn decode_rejects_bad_input() {
        assert_eq!(decode_base32_sort("short"), None);
        assert_eq!(decode_base32_sort("1111111111111"), None);
        assert_eq!(tid_to_unix_microseconds("nope"), None);
    }

    #[test]
    fn microseconds_round_trip_and_drop_the_clock_id() {
        let us = 1_757_775_845_000_000;
        assert_eq!(
            tid_to_unix_microseconds(&tid_from_unix_microseconds(us)),
            Some(us)
        );
        let with_clock = tid_from_parts(us as u64, 0x3FF);
        assert_eq!(tid_to_unix_microseconds(&with_clock), Some(us));
        assert_ne!(with_clock, tid_from_unix_microseconds(us));
    }

    #[test]
    fn clock_id_is_masked_to_ten_bits() {
        assert_eq!(tid_from_parts(1, 0xFFFF), tid_from_parts(1, 0x3FF));
    }

    #[test]
    fn later_timestamps_sort_later() {
        assert!(tid_from_unix_microseconds(2_000) > tid_from_unix_microseconds(1_000));
    }
}
