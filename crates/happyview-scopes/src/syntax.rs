//! Scope-string syntax: `resource[:positional][?params]`.
//!
//! Ported from `@atproto/oauth-scopes`' `ScopeStringSyntax` and `Parser`. The
//! two rules that HappyView's hand-rolled parsers got wrong both live here:
//!
//! 1. Repeated parameters are expressed by repeating the key
//!    (`?action=create&action=update`), exactly as `URLSearchParams.getAll`
//!    reads them. A comma-joined list is **one value**, and every known
//!    validator rejects it.
//! 2. An unknown parameter key invalidates the whole scope rather than being
//!    ignored.

/// The result of looking up a parameter declared single-valued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleParam<'a> {
    /// The key is not present.
    Absent,
    /// Exactly one value.
    One(&'a str),
    /// The key appeared more than once, which invalidates the scope.
    Repeated,
}

impl<'a> SingleParam<'a> {
    /// `None` for both absent and repeated — for callers that have already
    /// established the key is optional and treat a repeat as "no valid value".
    pub fn value(self) -> Option<&'a str> {
        match self {
            Self::One(v) => Some(v),
            _ => None,
        }
    }

    /// `None` when the key repeated, otherwise the (possibly absent) value.
    /// Lets a caller propagate invalidity with `?`.
    pub fn ok(self) -> Option<Option<&'a str>> {
        match self {
            Self::Absent => Some(None),
            Self::One(v) => Some(Some(v)),
            Self::Repeated => None,
        }
    }
}

/// A parsed but not yet validated scope string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSyntax {
    pub prefix: String,
    pub positional: Option<String>,
    /// Insertion-ordered key/value pairs, keys may repeat.
    pub params: Vec<(String, String)>,
    /// True when the string carried a `?` at all, even with no pairs after it.
    has_query: bool,
}

/// Decode one `application/x-www-form-urlencoded` component: `+` is a space,
/// `%XX` is a byte. Matches `URLSearchParams`, which is what the reference uses.
fn decode_form(value: &str) -> String {
    let plus_decoded = value.replace('+', " ");
    urlencoding::decode(&plus_decoded)
        .map(|c| c.into_owned())
        .unwrap_or(plus_decoded)
}

/// Decode a positional component. The reference uses `decodeURIComponent` here,
/// **not** `URLSearchParams`, so `+` is a literal plus rather than a space.
fn decode_uri_component(value: &str) -> String {
    urlencoding::decode(value)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| value.to_string())
}

fn min_idx(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

impl ScopeSyntax {
    pub fn parse(scope: &str) -> Self {
        let param_idx = scope.find('?');
        let colon_idx = scope.find(':');
        let prefix_end = min_idx(param_idx, colon_idx);

        let Some(prefix_end) = prefix_end else {
            return Self {
                prefix: scope.to_string(),
                positional: None,
                params: Vec::new(),
                has_query: false,
            };
        };

        let prefix = scope[..prefix_end].to_string();

        let positional = match (colon_idx, param_idx) {
            (Some(c), None) => Some(decode_uri_component(&scope[c + 1..])),
            (Some(c), Some(p)) if c < p => Some(decode_uri_component(&scope[c + 1..p])),
            // A `?` before the `:` means the colon is inside the query string,
            // not a positional separator.
            _ => None,
        };

        // The reference only builds params when the `?` is not the final
        // character, so a trailing `?` yields no params at all.
        let (params, has_query) = match param_idx {
            Some(p) if p < scope.len() - 1 => (parse_query(&scope[p + 1..]), true),
            Some(_) => (Vec::new(), false),
            None => (Vec::new(), false),
        };

        Self {
            prefix,
            positional,
            params,
            has_query,
        }
    }

    pub fn has_query(&self) -> bool {
        self.has_query
    }

    /// Every distinct key present, in first-appearance order.
    pub fn keys(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for (k, _) in &self.params {
            if !out.contains(&k.as_str()) {
                out.push(k);
            }
        }
        out
    }

    /// Look up a single-valued parameter. A repeated key is not "the last one
    /// wins" — the reference treats it as invalidating the whole scope, so the
    /// three states are kept distinct rather than collapsed into an `Option`.
    pub fn get_single(&self, key: &str) -> SingleParam<'_> {
        let mut found: Option<&str> = None;
        for (k, v) in &self.params {
            if k == key {
                if found.is_some() {
                    return SingleParam::Repeated;
                }
                found = Some(v);
            }
        }
        match found {
            Some(v) => SingleParam::One(v),
            None => SingleParam::Absent,
        }
    }

    /// `None` = absent, `Some(values)` = every occurrence in order.
    pub fn get_multi(&self, key: &str) -> Option<Vec<&str>> {
        let values: Vec<&str> = self
            .params
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect();
        if values.is_empty() {
            None
        } else {
            Some(values)
        }
    }
}

/// `URLSearchParams`-equivalent query parsing: split on `&`, then on the first
/// `=`. A pair with no `=` has an empty value; an empty pair is skipped.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (decode_form(k), decode_form(v)),
            None => (decode_form(pair), String::new()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_prefix() {
        let s = ScopeSyntax::parse("atproto");
        assert_eq!(s.prefix, "atproto");
        assert_eq!(s.positional, None);
        assert!(s.params.is_empty());
    }

    #[test]
    fn positional_only() {
        let s = ScopeSyntax::parse("repo:com.example.post");
        assert_eq!(s.prefix, "repo");
        assert_eq!(s.positional.as_deref(), Some("com.example.post"));
        assert!(s.params.is_empty());
    }

    #[test]
    fn positional_and_params() {
        let s = ScopeSyntax::parse("repo:com.example.post?action=create&action=update");
        assert_eq!(s.prefix, "repo");
        assert_eq!(s.positional.as_deref(), Some("com.example.post"));
        assert_eq!(s.get_multi("action"), Some(vec!["create", "update"]));
    }

    /// The divergence that motivated this crate: a comma-joined list is one
    /// value, so downstream validation rejects it.
    #[test]
    fn comma_joined_actions_are_a_single_value() {
        let s = ScopeSyntax::parse("repo:com.example.post?action=create,update");
        assert_eq!(s.get_multi("action"), Some(vec!["create,update"]));
    }

    #[test]
    fn params_without_positional() {
        let s = ScopeSyntax::parse("repo?collection=com.example.post");
        assert_eq!(s.prefix, "repo");
        assert_eq!(s.positional, None);
        assert_eq!(s.get_multi("collection"), Some(vec!["com.example.post"]));
    }

    #[test]
    fn get_single_rejects_repeats() {
        let s = ScopeSyntax::parse("account:email?action=read&action=manage");
        assert_eq!(s.get_single("action"), SingleParam::Repeated);
    }

    #[test]
    fn percent_decoding_in_positional() {
        // `#` must be encoded in a scope string; it survives decoding.
        let s = ScopeSyntax::parse("rpc:com.example.doThing?aud=did:web:x.com%23svc");
        assert_eq!(s.get_single("aud"), SingleParam::One("did:web:x.com#svc"));
    }

    #[test]
    fn colon_after_question_mark_is_not_positional() {
        let s = ScopeSyntax::parse("rpc?aud=did:web:example.com&lxm=com.example.a");
        assert_eq!(s.prefix, "rpc");
        assert_eq!(s.positional, None);
        assert_eq!(s.get_single("aud"), SingleParam::One("did:web:example.com"));
    }

    #[test]
    fn trailing_question_mark_yields_no_params() {
        let s = ScopeSyntax::parse("repo:com.example.post?");
        assert!(s.params.is_empty());
        assert!(!s.has_query());
    }

    #[test]
    fn keys_are_deduped_in_order() {
        let s = ScopeSyntax::parse("repo?collection=a.b.c&action=create&collection=d.e.f");
        assert_eq!(s.keys(), vec!["collection", "action"]);
    }
}
