//! The encodings the compact record format stores, each with an exact
//! inverse, so what raw SQL reads back through the compatibility views is
//! byte-for-byte what was written:
//!
//! * a CID as bytes: a canonical base32 CIDv1 (`b…`) as its binary form,
//!   the empty string as no bytes, and any other text (a test's `c1`, a
//!   CIDv0) behind a `0x00` marker byte, which no CIDv1 starts with;
//! * a timestamp as unix microseconds, rendered back as RFC 3339 in UTC;
//! * a record URI as its three parts, and a ref target as three parts or,
//!   when it is not a record URI, as text the view can rebuild exactly.
//!
//! `sqlite_functions` and `migrations/postgres/20261011000000_record_store_functions.sql`
//! implement the same rules in SQL for the views and triggers; tests hold
//! the three to agreement.

use chrono::{DateTime, NaiveDate, Utc};

const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Marker byte for a CID stored as its text.
const CID_TEXT_MARKER: u8 = 0x00;

/// `target_collection_id` for a ref target that is `at://` followed by the
/// interned text.
pub const REF_TARGET_AT: i64 = 0;

/// `target_collection_id` for a ref target stored verbatim.
pub const REF_TARGET_TEXT: i64 = -1;

/// RFC 4648 base32, lowercase, unpadded: the multibase `b` alphabet.
pub fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    for &byte in bytes {
        buffer = (buffer << 8) | u64::from(byte);
        bits += 8;
        while bits >= 5 {
            out.push(BASE32[((buffer >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(BASE32[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The inverse of [`base32_encode`]. Leftover bits are dropped, so a caller
/// that needs canonical input re-encodes and compares.
pub fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    for c in text.bytes() {
        let value = BASE32.iter().position(|&a| a == c)? as u64;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            out.push(((buffer >> (bits - 8)) & 0xff) as u8);
            bits -= 8;
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub fn encode_cid(text: &str) -> Vec<u8> {
    if text.is_empty() {
        return Vec::new();
    }
    if let Some(rest) = text.strip_prefix('b')
        && let Some(bytes) = base32_decode(rest)
        && bytes.first() == Some(&0x01)
        && base32_encode(&bytes) == rest
    {
        return bytes;
    }
    let mut out = Vec::with_capacity(text.len() + 1);
    out.push(CID_TEXT_MARKER);
    out.extend_from_slice(text.as_bytes());
    out
}

pub fn decode_cid(bytes: &[u8]) -> String {
    match bytes.first() {
        None => String::new(),
        Some(&CID_TEXT_MARKER) => String::from_utf8_lossy(&bytes[1..]).into_owned(),
        Some(_) => format!("b{}", base32_encode(bytes)),
    }
}

/// The first instant a timestamp can hold, `0000-01-01T00:00:00Z`.
pub const MIN_MICROS: i64 = -62_167_219_200_000_000;

/// The last instant a timestamp can hold, `9999-12-31T23:59:59.999999Z`.
pub const MAX_MICROS: i64 = 253_402_300_799_999_999;

/// A stored timestamp in any form HappyView has written: RFC 3339 at any
/// precision and offset, SQLite's `datetime('now')` (UTC), or Postgres's
/// `timestamptz::text` (`+00` offsets).
///
/// The grammar is explicit, because `hv_ts_micros` on Postgres repeats it
/// and the two must agree on every input:
///
/// * `YYYY-MM-DD`, then `T`, `t` or a space, then `HH:MM:SS`, every field
///   ASCII digits of exactly that width. `SS` may be `60`, a leap second,
///   read as the next second, as Postgres reads it.
/// * Optionally `.` and one or more digits. Digits past the ninth are
///   ignored; the rest rounds half up to the microsecond.
/// * Optionally `Z`, `z`, or `+`/`-` with `HH`, `HHMM` or `HH:MM` (up to
///   23:59). No offset means UTC.
/// * The instant must fall in years 0000 to 9999 ([`MIN_MICROS`] to
///   [`MAX_MICROS`]), the span the rendered form can spell.
pub fn parse_micros(text: &str) -> Option<i64> {
    fn digits(bytes: &[u8]) -> Option<i64> {
        if bytes.is_empty() {
            return None;
        }
        bytes.iter().try_fold(0i64, |n, &b| {
            b.is_ascii_digit().then(|| n * 10 + i64::from(b - b'0'))
        })
    }

    let b = text.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let field = |at: usize, len: usize| digits(&b[at..at + len]);
    let (year, month, day) = (field(0, 4)?, field(5, 2)?, field(8, 2)?);
    let (hour, minute, second) = (field(11, 2)?, field(14, 2)?, field(17, 2)?);
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(year as i32, month as u32, day as u32)?;
    let days = (date - DateTime::UNIX_EPOCH.date_naive()).num_days();

    let mut rest = &b[19..];
    let mut nanos = 0;
    if let Some(fraction) = rest.strip_prefix(b".") {
        let len = fraction.iter().take_while(|c| c.is_ascii_digit()).count();
        let kept = &fraction[..len.min(9)];
        nanos = digits(kept)? * 10i64.pow(9 - kept.len() as u32);
        rest = &fraction[len..];
    }
    let offset = match rest {
        [] | [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), zone @ ..] => {
            let (h, m) = match zone {
                [_, _] => (digits(zone)?, 0),
                [h1, h2, m1, m2] | [h1, h2, b':', m1, m2] => {
                    (digits(&[*h1, *h2])?, digits(&[*m1, *m2])?)
                }
                _ => return None,
            };
            if h > 23 || m > 59 {
                return None;
            }
            if *sign == b'-' {
                -(h * 3_600 + m * 60)
            } else {
                h * 3_600 + m * 60
            }
        }
        _ => return None,
    };

    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset;
    let us = seconds * 1_000_000 + (nanos + 500) / 1_000;
    (MIN_MICROS..=MAX_MICROS).contains(&us).then_some(us)
}

/// RFC 3339 in UTC with six fractional digits, which [`parse_micros`] reads
/// back exactly. Nothing HappyView writes falls outside years 0000 to 9999,
/// so a value that does is corrupt: it renders as `invalid timestamp {us}`,
/// visibly wrong and still showing the stored number, rather than as a
/// plausible date. `hv_ts_text` renders the same on both backends.
pub fn render_micros(us: i64) -> String {
    match DateTime::from_timestamp_micros(us) {
        Some(dt) if (MIN_MICROS..=MAX_MICROS).contains(&us) => {
            dt.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
        }
        _ => format!("invalid timestamp {us}"),
    }
}

pub fn now_micros() -> i64 {
    Utc::now().timestamp_micros()
}

/// `at://{did}/{collection}/{rkey}` with three non-empty parts and nothing
/// after the rkey: the only form a stored record's URI takes.
pub fn parse_record_uri(uri: &str) -> Option<(&str, &str, &str)> {
    let rest = uri.strip_prefix("at://")?;
    let mut parts = rest.split('/');
    let (did, collection, rkey) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || did.is_empty() || collection.is_empty() || rkey.is_empty() {
        return None;
    }
    Some((did, collection, rkey))
}

/// Where a ref points. Record refs intern their parts; anything else a
/// record body holds (`at://did:plc:x`, a handle URI with a trailing path)
/// or raw SQL inserted keeps its exact text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefTarget<'a> {
    Record {
        did: &'a str,
        collection: &'a str,
        rkey: &'a str,
    },
    /// The text after `at://`.
    At(&'a str),
    Text(&'a str),
}

impl<'a> RefTarget<'a> {
    pub fn parse(target: &'a str) -> Self {
        if let Some((did, collection, rkey)) = parse_record_uri(target) {
            return Self::Record {
                did,
                collection,
                rkey,
            };
        }
        match target.strip_prefix("at://") {
            Some(rest) => Self::At(rest),
            None => Self::Text(target),
        }
    }

    /// The text interned in `happyview_dids` for this target.
    pub fn did_text(&self) -> &'a str {
        match self {
            Self::Record { did, .. } => did,
            Self::At(rest) => rest,
            Self::Text(text) => text,
        }
    }

    pub fn collection(&self) -> Option<&'a str> {
        match self {
            Self::Record { collection, .. } => Some(collection),
            Self::At(_) | Self::Text(_) => None,
        }
    }

    pub fn rkey(&self) -> &'a str {
        match self {
            Self::Record { rkey, .. } => rkey,
            Self::At(_) | Self::Text(_) => "",
        }
    }

    /// `1` for a record, else the sentinel stored as `target_collection_id`.
    pub fn kind(&self) -> i64 {
        match self {
            Self::Record { .. } => 1,
            Self::At(_) => REF_TARGET_AT,
            Self::Text(_) => REF_TARGET_TEXT,
        }
    }

    pub fn render(&self) -> String {
        match self {
            Self::Record {
                did,
                collection,
                rkey,
            } => format!("at://{did}/{collection}/{rkey}"),
            Self::At(rest) => format!("at://{rest}"),
            Self::Text(text) => (*text).to_string(),
        }
    }
}

/// `hv_ref_part(target, part)`: `0` the interned text, `1` the collection
/// (NULL unless a record), `2` the rkey (`''` unless a record), `3` the kind
/// as text.
pub fn ref_part(target: &str, part: i64) -> Option<String> {
    let parsed = RefTarget::parse(target);
    match part {
        0 => Some(parsed.did_text().to_string()),
        1 => parsed.collection().map(str::to_string),
        2 => Some(parsed.rkey().to_string()),
        3 => Some(parsed.kind().to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator, so a failing case reproduces.
    struct Xorshift(u64);
    impl Xorshift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    const POST_CID: &str = "bafyreie5737gdxlw5i64vzichcalba3z2v5n6icifvx5xytvske7mr3hpm";

    #[test]
    fn a_cidv1_is_stored_as_its_36_binary_bytes() {
        let bytes = encode_cid(POST_CID);
        assert_eq!(bytes.len(), 36);
        assert_eq!(&bytes[..4], &[0x01, 0x71, 0x12, 0x20]);
        assert_eq!(decode_cid(&bytes), POST_CID);
    }

    #[test]
    fn any_other_cid_text_round_trips_behind_a_marker() {
        for text in [
            "",
            "c50",
            "b",
            "bafy",
            "bafyBAD",
            "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG",
            "é",
        ] {
            let bytes = encode_cid(text);
            assert_eq!(decode_cid(&bytes), text, "{text:?}");
            if !text.is_empty() {
                assert_eq!(bytes[0], 0x00, "{text:?} is stored as text");
            }
        }
    }

    #[test]
    fn random_cids_and_strings_round_trip() {
        let mut rng = Xorshift(0x5eed_1234_abcd_ef01);
        for _ in 0..2000 {
            let len = (rng.next() % 40) as usize + 1;
            let mut bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            bytes[0] = 0x01;
            let text = format!("b{}", base32_encode(&bytes));
            assert_eq!(encode_cid(&text), bytes);
            assert_eq!(decode_cid(&encode_cid(&text)), text);

            let s: String = (0..len)
                .map(|_| char::from_u32((rng.next() % 0x2FF) as u32 + 0x20).unwrap_or('x'))
                .collect();
            assert_eq!(decode_cid(&encode_cid(&s)), s, "{s:?}");
        }
    }

    /// Expected values were read back from Postgres 16's `hv_ts_micros` and
    /// `hv_ts_text` (Task 6), so the Rust and SQL codecs agree on them.
    #[test]
    fn timestamps_parse_to_microseconds_and_render_in_utc() {
        for (input, micros, rendered) in [
            (
                "2026-10-11T12:34:56.123456789+00:00",
                1_791_722_096_123_457,
                "2026-10-11T12:34:56.123457+00:00",
            ),
            (
                "2026-03-16 12:34:56",
                1_773_664_496_000_000,
                "2026-03-16T12:34:56.000000+00:00",
            ),
            (
                "2026-03-18 12:00:00.123456+00",
                1_773_835_200_123_456,
                "2026-03-18T12:00:00.123456+00:00",
            ),
            (
                "2026-01-01T00:00:00Z",
                1_767_225_600_000_000,
                "2026-01-01T00:00:00.000000+00:00",
            ),
            (
                "1969-12-31T23:59:59.5+00:00",
                -500_000,
                "1969-12-31T23:59:59.500000+00:00",
            ),
            (
                "2026-10-11T08:34:56.5-04:00",
                1_791_722_096_500_000,
                "2026-10-11T12:34:56.500000+00:00",
            ),
        ] {
            assert_eq!(parse_micros(input), Some(micros), "{input}");
            assert_eq!(render_micros(micros), rendered, "{input}");
            assert_eq!(
                parse_micros(rendered),
                Some(micros),
                "rendered form of {input}"
            );
        }
        assert_eq!(parse_micros("yesterday"), None);
        assert_eq!(parse_micros(""), None);
    }

    /// The grammar `hv_ts_micros` on Postgres repeats, so its edges are
    /// pinned here; `sqlite_functions` checks the SQL against these.
    #[test]
    fn the_timestamp_grammar_is_exact() {
        const T: i64 = 1_791_722_096_000_000; // 2026-10-11T12:34:56Z
        for (input, micros) in [
            ("2026-10-11T12:34:56", T),
            ("2026-10-11t12:34:56z", T),
            ("2026-10-11 12:34:56Z", T),
            ("2026-10-11T18:04:56+0530", T),
            ("2026-10-11T18:04:56+05:30", T),
            ("2026-10-11T08:34:56-04", T),
            ("2026-10-11T12:34:56-00:00", T),
            ("2026-10-11T12:34:56.0000005Z", T + 1),
            ("2026-10-11T12:34:56.0000004999Z", T),
            ("2026-10-11T12:34:55.9999995Z", T),
            ("1969-12-31T23:59:59.0000005Z", -999_999),
            ("1969-12-31T23:59:59.9999995Z", 0),
            ("2016-12-31T23:59:60Z", 1_483_228_800_000_000),
            ("0000-01-01T00:00:00Z", MIN_MICROS),
            ("9999-12-31T23:59:59.999999Z", MAX_MICROS),
        ] {
            assert_eq!(parse_micros(input), Some(micros), "{input}");
        }
        for bad in [
            " 2026-10-11 12:34:56",
            "2026-10-11 12:34:56 ",
            "2026-1-11 12:34:56",
            "+2026-10-11 12:34:56",
            "2026-02-29 00:00:00",
            "2026-10-11T24:00:00Z",
            "2026-10-11T12:34:56 +00:00",
            "2026-10-11T12:34:56+24:00",
            "2026-10-11T12:34:56+05:30:00",
            "2026-10-11T12:34:56.Z",
            "9999-12-31T23:59:59.9999995Z",
            "0000-01-01T00:00:00+00:01",
            "now",
        ] {
            assert_eq!(parse_micros(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_timestamp_outside_years_0000_to_9999_renders_as_invalid() {
        assert_eq!(
            render_micros(MIN_MICROS),
            "0000-01-01T00:00:00.000000+00:00"
        );
        assert_eq!(
            render_micros(MAX_MICROS),
            "9999-12-31T23:59:59.999999+00:00"
        );
        assert_eq!(
            render_micros(i64::MAX),
            "invalid timestamp 9223372036854775807"
        );
        assert_eq!(
            render_micros(MIN_MICROS - 1),
            "invalid timestamp -62167219200000001"
        );
        assert_eq!(parse_micros(&render_micros(i64::MIN)), None);
    }

    #[test]
    fn a_record_uri_has_exactly_three_nonempty_parts() {
        assert_eq!(
            parse_record_uri("at://did:plc:a/app.bsky.feed.post/3l"),
            Some(("did:plc:a", "app.bsky.feed.post", "3l"))
        );
        for bad in [
            "at://did:plc:a",
            "at://did:plc:a/c",
            "at://did:plc:a/c/",
            "at://did:plc:a//r",
            "at://a/c/r/x",
            "did:plc:a/c/r",
            "at:///c/r",
        ] {
            assert_eq!(parse_record_uri(bad), None, "{bad}");
        }
    }

    #[test]
    fn every_ref_target_renders_back_to_itself() {
        for target in [
            "at://did:plc:a/app.bsky.feed.post/3l",
            "at://did:plc:a",
            "at://did:plc:a/",
            "at://alice.example.com/app.bsky.feed.post/3l/extra",
            "at://",
            "https://example.com",
            "",
        ] {
            let parsed = RefTarget::parse(target);
            assert_eq!(parsed.render(), target, "{target:?}");
            assert_eq!(
                ref_part(target, 3).as_deref(),
                Some(parsed.kind().to_string().as_str())
            );
        }
        assert_eq!(RefTarget::parse("at://did:plc:a").kind(), REF_TARGET_AT);
        assert_eq!(RefTarget::parse("https://x").kind(), REF_TARGET_TEXT);
        assert_eq!(ref_part("at://did:plc:a/c/r", 1).as_deref(), Some("c"));
        assert_eq!(ref_part("at://did:plc:a", 1), None);
        assert_eq!(ref_part("at://did:plc:a", 0).as_deref(), Some("did:plc:a"));
        assert_eq!(ref_part("x", 9), None);
    }

    #[test]
    fn rendering_then_parsing_is_the_identity_on_microseconds() {
        let mut rng = Xorshift(0x1234_5678_9abc_def1);
        for _ in 0..2000 {
            let us = (rng.next() % 4_000_000_000_000_000) as i64 - 1_000_000_000_000_000;
            assert_eq!(parse_micros(&render_micros(us)), Some(us), "{us}");
        }
        assert!(parse_micros(&render_micros(now_micros())).is_some());
    }

    /// The forms `indexed_at` and `created_at` are written in today. Every one
    /// round-trips to the same instant; only a rendered-form input is
    /// byte-identical, which is what `format_drift_of_stored_timestamps`
    /// documents.
    #[test]
    fn stored_timestamp_formats_keep_their_instant() {
        // `db::now_rfc3339()`, i.e. `Utc::now().to_rfc3339()`: nanoseconds.
        let written = Utc::now().to_rfc3339();
        let us = parse_micros(&written).expect("rfc3339 parses");
        assert_eq!(parse_micros(&render_micros(us)), Some(us));
        // SQLite `datetime('now')` column default: seconds, no offset.
        assert_eq!(
            render_micros(parse_micros("2026-10-11 12:34:56").unwrap()),
            "2026-10-11T12:34:56.000000+00:00"
        );
        // Postgres `timestamptz::text` in a UTC session, trailing zeros trimmed.
        assert_eq!(
            parse_micros("2026-10-11 12:34:56.5+00"),
            Some(1_791_722_096_500_000)
        );
        // Postgres with a non-UTC session timezone.
        assert_eq!(
            parse_micros("2026-10-11 08:34:56.5-04"),
            Some(1_791_722_096_500_000)
        );
        assert_eq!(
            parse_micros("2026-10-11 18:04:56.5+05:30"),
            Some(1_791_722_096_500_000)
        );
    }

    #[test]
    fn format_drift_of_stored_timestamps() {
        for input in [
            "2026-10-11T12:34:56.123456789+00:00", // now_rfc3339(): 9 digits
            "2026-10-11T12:34:56+00:00",           // whole second
            "2026-10-11 12:34:56",                 // datetime('now')
            "2026-10-11 12:34:56.123457+00",       // Postgres text
            "2026-10-11T12:34:56.123Z",
        ] {
            let rendered = render_micros(parse_micros(input).unwrap());
            assert_ne!(
                rendered, input,
                "{input} is not byte-stable through the codec"
            );
        }
        assert_eq!(
            render_micros(parse_micros("2026-10-11T12:34:56.123457+00:00").unwrap()),
            "2026-10-11T12:34:56.123457+00:00"
        );
    }
}
