use happyview_nsid::validate_nsid;

const MAX_SEGMENT_LEN: usize = 63;

/// Which lexicons a publishing rule covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    All,
    /// Lowercased, without the trailing `.*`.
    Prefix(String),
    Exact(String),
}

impl Pattern {
    pub fn parse(raw: &str) -> Result<Pattern, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("pattern is empty".to_string());
        }
        if raw == "*" {
            return Ok(Pattern::All);
        }
        if let Some(base) = raw.strip_suffix(".*") {
            // DNS names are case-insensitive, so the authority is stored in
            // one case and matched against a lowercased NSID.
            let base = base.to_ascii_lowercase();
            if base.is_empty() {
                return Err("pattern has nothing before '.*'".to_string());
            }
            for segment in base.split('.') {
                validate_segment(segment)?;
            }
            return Ok(Pattern::Prefix(base));
        }
        if raw.contains('*') {
            return Err("'*' is only allowed alone or as a trailing '.*'".to_string());
        }
        validate_nsid(raw).map_err(|e| e.to_string())?;
        Ok(Pattern::Exact(raw.to_string()))
    }

    pub fn as_stored(&self) -> String {
        match self {
            Pattern::All => "*".to_string(),
            Pattern::Prefix(base) => format!("{base}.*"),
            Pattern::Exact(nsid) => nsid.clone(),
        }
    }

    pub fn matches(&self, nsid: &str) -> bool {
        match self {
            Pattern::All => true,
            Pattern::Prefix(base) => {
                let lowered = nsid.to_ascii_lowercase();
                lowered
                    .strip_prefix(base.as_str())
                    .is_some_and(|rest| rest.starts_with('.'))
            }
            Pattern::Exact(exact) => exact == nsid,
        }
    }

    /// Higher wins: an exact rule beats any prefix, a longer prefix beats a
    /// shorter one, and `*` is the fallback.
    pub fn specificity(&self) -> (u8, usize) {
        match self {
            Pattern::All => (0, 0),
            Pattern::Prefix(base) => (1, base.split('.').count()),
            Pattern::Exact(nsid) => (2, nsid.split('.').count()),
        }
    }
}

fn validate_segment(segment: &str) -> Result<(), String> {
    if segment.is_empty() {
        return Err("pattern has an empty segment".to_string());
    }
    if segment.len() > MAX_SEGMENT_LEN {
        return Err(format!("segment '{segment}' is longer than 63 characters"));
    }
    if !segment
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(format!(
            "segment '{segment}' takes only ASCII letters, digits, and hyphens"
        ));
    }
    if segment.starts_with('-') || segment.ends_with('-') {
        return Err(format!(
            "segment '{segment}' must not start or end with a hyphen"
        ));
    }
    Ok(())
}

/// The rule with the highest specificity that matches `nsid`. Equal
/// specificity keeps the earlier rule, so callers order ties themselves.
pub fn best_match<'a, T>(
    nsid: &str,
    rules: &'a [T],
    pattern_of: impl Fn(&T) -> &Pattern,
) -> Option<&'a T> {
    let mut best: Option<&'a T> = None;
    for rule in rules {
        let pattern = pattern_of(rule);
        if !pattern.matches(nsid) {
            continue;
        }
        if best.is_none_or(|b| pattern.specificity() > pattern_of(b).specificity()) {
            best = Some(rule);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_forms() {
        assert_eq!(Pattern::parse("*"), Ok(Pattern::All));
        assert_eq!(
            Pattern::parse("com.example.*"),
            Ok(Pattern::Prefix("com.example".into()))
        );
        assert_eq!(
            Pattern::parse("com.example.foo.bar"),
            Ok(Pattern::Exact("com.example.foo.bar".into()))
        );
        assert_eq!(Pattern::parse("com.*"), Ok(Pattern::Prefix("com".into())));
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(
            Pattern::parse("  com.example.*\n"),
            Ok(Pattern::Prefix("com.example".into()))
        );
    }

    #[test]
    fn normalises_prefix_case_and_matches_mixed_case_nsids() {
        let p = Pattern::parse("COM.Example.*").unwrap();
        assert_eq!(p.as_stored(), "com.example.*");
        assert!(p.matches("COM.Example.foo.bar"));
        assert!(p.matches("com.example.foo.bar"));
    }

    #[test]
    fn exact_is_case_sensitive() {
        let p = Pattern::parse("com.example.fooBar").unwrap();
        assert!(p.matches("com.example.fooBar"));
        assert!(!p.matches("com.example.foobar"));
        assert_eq!(p.as_stored(), "com.example.fooBar");
    }

    #[test]
    fn refuses_bad_input() {
        for bad in [
            "",
            "  ",
            "**",
            "com.*.foo",
            "com..foo.*",
            "com.example.",
            "-bad.*",
            "com",
            ".*",
            "*.com",
        ] {
            assert!(Pattern::parse(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn prefix_respects_segment_boundaries() {
        let p = Pattern::parse("com.example.*").unwrap();
        assert!(p.matches("com.example.foo"));
        assert!(!p.matches("com.examples.foo"));
        assert!(!p.matches("com.example"));
    }

    #[test]
    fn as_stored_round_trips() {
        for s in ["*", "com.example.*", "com.example.foo"] {
            assert_eq!(Pattern::parse(s).unwrap().as_stored(), s);
        }
    }

    #[test]
    fn precedence_is_exact_then_longer_prefix_then_shorter_then_all() {
        let rules: Vec<(&str, Pattern)> = ["*", "com.*", "com.example.*", "com.example.foo.bar"]
            .into_iter()
            .map(|s| (s, Pattern::parse(s).unwrap()))
            .collect();
        let pick = |nsid: &str| best_match(nsid, &rules, |r| &r.1).map(|r| r.0);
        assert_eq!(pick("com.example.foo.bar"), Some("com.example.foo.bar"));
        assert_eq!(pick("com.example.foo.baz"), Some("com.example.*"));
        assert_eq!(pick("com.other.foo.baz"), Some("com.*"));
        assert_eq!(pick("org.other.foo.baz"), Some("*"));

        let no_all = &rules[1..];
        assert_eq!(best_match("org.x.y.z", no_all, |r| &r.1), None);
    }
}
