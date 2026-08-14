//! AT Protocol NSID validation.
//!
//! This crate owns the spec's canonical regex verbatim rather than delegating to
//! `atrium-api` or `jacquard-common`. Both of those are correct, and both cost
//! more than a megabyte of generated code and a dozen transitive dependencies to
//! reach the same three lines. See
//! `.claude/plans/2026-08-14-nsid-consolidation-design.md`.
//!
//! Correctness is guaranteed by the vendored interop corpus in `tests/`, not by
//! trusting this file.

use std::sync::OnceLock;

use regex_lite::Regex;

/// The canonical NSID regex, copied verbatim from
/// <https://atproto.com/specs/nsid>.
///
/// Only the first (TLD) and last (name) segments must start with a letter; the
/// authority segments between them are reversed domain labels and may start with
/// a digit. This is the rule HappyView used to get wrong.
///
/// This string is byte-identical to the `NSID_PATTERN` exported by
/// `@happyview/nsid`.
///
/// `web/src/lib/lexicon-schema.ts` is a third copy that is currently
/// *functionally* equivalent but not byte-identical: it spells the name
/// segment `(\.[a-zA-Z]([a-zA-Z0-9]{0,62})?)$`, the spec's literal text,
/// where this uses `(\.[a-zA-Z][a-zA-Z0-9]{0,62})$`, the form atrium and
/// jacquard use. Both match the same set of strings — `{0,62}` already
/// permits zero — so this is a spelling difference, not a behavioural one.
/// Task 11 of the consolidation plan replaces that inline string with an
/// import of `NSID_PATTERN`, which is what finally makes all three identical.
pub const NSID_PATTERN: &str = r"^[a-zA-Z]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)+(\.[a-zA-Z][a-zA-Z0-9]{0,62})$";

/// Maximum total NSID length. The pattern bounds each segment but not the total,
/// so this is checked separately.
pub const MAX_NSID_LEN: usize = 317;

fn nsid_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(NSID_PATTERN).expect("NSID_PATTERN is a valid regex"))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NsidError {
    // The 317 is written out rather than interpolated from MAX_NSID_LEN:
    // thiserror resolves named arguments in the format string against the
    // variant's fields, and a bare `{MAX_NSID_LEN}` is not a field.
    #[error("invalid NSID '{0}': must be at most 317 characters")]
    TooLong(String),

    #[error(
        "invalid NSID '{0}': must be at least three dot-separated segments, where only the \
         first and last must start with a letter and the last takes no hyphens \
         (see https://atproto.com/specs/nsid)"
    )]
    Malformed(String),

    #[error("invalid NSID pattern '{0}': {1}")]
    Pattern(String, &'static str),
}

/// Validates a complete NSID against the AT Protocol spec.
pub fn validate_nsid(nsid: &str) -> Result<(), NsidError> {
    if nsid.len() > MAX_NSID_LEN {
        return Err(NsidError::TooLong(nsid.to_string()));
    }
    if !nsid_regex().is_match(nsid) {
        return Err(NsidError::Malformed(nsid.to_string()));
    }
    Ok(())
}

/// Maximum length of a single dot-separated segment.
const MAX_SEGMENT_LEN: usize = 63;

/// Validates a proxy-config NSID pattern: either an exact NSID, or an authority
/// prefix followed by `.*`.
///
/// The wildcard form cannot delegate to [`validate_nsid`] — the base of
/// `com.example.*` is `com.example`, a reversed domain prefix rather than a
/// complete NSID, so it is validated under domain-label rules instead.
pub fn validate_nsid_pattern(pattern: &str) -> Result<(), NsidError> {
    let Some(base) = pattern.strip_suffix(".*") else {
        return validate_nsid(pattern);
    };

    if base.len() > MAX_NSID_LEN {
        return Err(NsidError::TooLong(pattern.to_string()));
    }

    let segments: Vec<&str> = base.split('.').collect();
    if segments.len() < 2 {
        return Err(NsidError::Pattern(
            pattern.to_string(),
            "a wildcard needs at least two leading segments",
        ));
    }

    for (idx, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            return Err(NsidError::Pattern(pattern.to_string(), "empty segment"));
        }
        if segment.len() > MAX_SEGMENT_LEN {
            return Err(NsidError::Pattern(
                pattern.to_string(),
                "segment is longer than 63 characters",
            ));
        }
        if !segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(NsidError::Pattern(
                pattern.to_string(),
                "segments take only ASCII letters, digits, and hyphens",
            ));
        }
        if segment.starts_with('-') || segment.ends_with('-') {
            return Err(NsidError::Pattern(
                pattern.to_string(),
                "segments must not start or end with a hyphen",
            ));
        }
        // Only the TLD carries the letter-initial rule; the segments after it
        // are domain labels and may start with a digit.
        if idx == 0 && !segment.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return Err(NsidError::Pattern(
                pattern.to_string(),
                "the first segment must start with a letter",
            ));
        }
    }

    Ok(())
}

/// Returns the NSID's authority: every segment except the name, reversed into a
/// domain name. `pics.2bit.feed.getPhotos` yields `feed.2bit.pics`.
///
/// Validates before deriving, so callers cannot build a lookup domain out of a
/// malformed NSID.
pub fn nsid_authority(nsid: &str) -> Result<String, NsidError> {
    validate_nsid(nsid)?;

    let mut segments: Vec<&str> = nsid.split('.').collect();
    segments.pop(); // the name segment
    segments.reverse();
    Ok(segments.join("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_digit_leading_authority_segments() {
        // The bug this crate exists to prevent: reported by @a2.co, who could
        // not attach scripts to `pics.2bit.feed.getPhotos`.
        assert!(validate_nsid("pics.2bit.feed.getPhotos").is_ok());
        assert!(validate_nsid("pics.2bit.photo").is_ok());
        assert!(validate_nsid("pics.2-bit.photo").is_ok());
    }

    #[test]
    fn rejects_segment_position_violations() {
        // TLD must start with a letter.
        assert!(validate_nsid("2bit.pics.photo").is_err());
        // So must the name segment, which also takes no hyphens.
        assert!(validate_nsid("pics.2bit.2photo").is_err());
        assert!(validate_nsid("pics.2bit.get-photos").is_err());
        // Fewer than three segments.
        assert!(validate_nsid("com.example").is_err());
    }

    #[test]
    fn enforces_the_length_cap() {
        let long = format!("com.{}.foo", "a".repeat(320));
        assert_eq!(validate_nsid(&long), Err(NsidError::TooLong(long)));
    }

    #[test]
    fn accepts_valid_patterns() {
        // Exact NSIDs.
        assert!(validate_nsid_pattern("com.example.feed.getHot").is_ok());
        assert!(validate_nsid_pattern("a.b.c").is_ok());
        // Wildcards over an authority prefix — two segments is fine here, because
        // the base is a reversed domain rather than a complete NSID.
        assert!(validate_nsid_pattern("com.example.*").is_ok());
        assert!(validate_nsid_pattern("games.gamesgamesgamesgames.*").is_ok());
        assert!(validate_nsid_pattern("pics.2bit.*").is_ok());
    }

    #[test]
    fn rejects_invalid_patterns() {
        assert!(validate_nsid_pattern("").is_err());
        assert!(validate_nsid_pattern("*").is_err());
        assert!(validate_nsid_pattern("com").is_err());
        assert!(validate_nsid_pattern("com.example.*.foo").is_err());
        assert!(validate_nsid_pattern("com..example").is_err());
        assert!(validate_nsid_pattern(".com.example").is_err());
        assert!(validate_nsid_pattern("com.example.").is_err());
    }

    #[test]
    fn pattern_prefix_rules_are_not_lax() {
        // These were all accepted by the old hand-rolled proxy_config validator.
        assert!(validate_nsid_pattern("1.foo.*").is_err());
        assert!(validate_nsid_pattern("com.foo-.*").is_err());
        assert!(validate_nsid_pattern("com.-foo.*").is_err());
    }

    #[test]
    fn rejects_oversized_pattern_segments() {
        // Both branches are reachable from the wildcard path and were untested:
        // the per-segment 63-char cap, and the whole-base 317-char cap. Asserting
        // on the specific variant (not just is_err()) pins which check fires.
        let long_segment = "a".repeat(64);
        assert!(matches!(
            validate_nsid_pattern(&format!("com.{long_segment}.*")),
            Err(NsidError::Pattern(_, _))
        ));

        // A base over MAX_NSID_LEN, built from legal 63-char segments so that
        // the length cap is what rejects it rather than a segment rule: "com."
        // plus six 63-char segments joined by dots is 387 characters, well
        // over 317, with every individual segment legal on its own.
        let seg = "a".repeat(63);
        let long_base = std::iter::repeat_n(seg.as_str(), 6)
            .collect::<Vec<_>>()
            .join(".");
        assert!(matches!(
            validate_nsid_pattern(&format!("com.{long_base}.*")),
            Err(NsidError::TooLong(_))
        ));
    }

    #[test]
    fn derives_the_reversed_authority_domain() {
        assert_eq!(
            nsid_authority("pics.2bit.feed.getPhotos").unwrap(),
            "feed.2bit.pics"
        );
        assert_eq!(nsid_authority("com.example.thing").unwrap(), "example.com");
    }

    #[test]
    fn authority_validates_before_deriving() {
        // The old resolve.rs check was `segments.len() < 2`, which accepted this.
        assert!(nsid_authority("1.foo").is_err());
        assert!(nsid_authority("single").is_err());
    }
}
