//! Walks the vendored AT Protocol interop corpus.
//!
//! The corpus deliberately contains leading- and trailing-whitespace cases:
//! `nsid_syntax_invalid.txt` holds both `one.two.three ` and ` one.two.three`,
//! whose trimmed form is a *valid* NSID listed in the other file. Trimming each
//! line silently turns those two cases into passes. Strip only the line
//! terminator.

use happyview_nsid::validate_nsid;

fn cases(raw: &str) -> Vec<&str> {
    raw.lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

#[test]
fn accepts_every_valid_interop_case() {
    let raw = include_str!("interop/nsid_syntax_valid.txt");
    let cases = cases(raw);
    assert_eq!(
        cases.len(),
        25,
        "corpus size changed; re-check the vendored file"
    );
    for case in cases {
        assert!(
            validate_nsid(case).is_ok(),
            "expected valid, was rejected: {case:?}"
        );
    }
}

#[test]
fn rejects_every_invalid_interop_case() {
    let raw = include_str!("interop/nsid_syntax_invalid.txt");
    let cases = cases(raw);
    assert_eq!(
        cases.len(),
        27,
        "corpus size changed; re-check the vendored file"
    );
    for case in cases {
        assert!(
            validate_nsid(case).is_err(),
            "expected invalid, was accepted: {case:?}"
        );
    }
}
