//! The one rendering rule between canonical snake_case names on the wire
//! and what a JavaScript script types. Canonical names never change; Lua and
//! Python use them verbatim, since snake_case is the norm in both. Only
//! JavaScript needs a rendering step, so it lives here for a JavaScript
//! interpreter to call.

use alloc::string::String;

/// `save_local` → `saveLocal`, `to_tid` → `toTid`, `records` → `records`.
/// Acronyms come out as words because the reverse direction has no way to
/// recover them, so the wire form is what carries meaning.
pub fn render_js_name(canonical: &str) -> String {
    let mut out = String::with_capacity(canonical.len());
    let mut upper_next = false;
    for c in canonical.chars() {
        if c == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(c.to_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_snake_case_as_camel_case() {
        assert_eq!(render_js_name("save_local"), "saveLocal");
        assert_eq!(render_js_name("to_tid"), "toTid");
        assert_eq!(render_js_name("to_iso8601"), "toIso8601");
        assert_eq!(render_js_name("get_labels_batch"), "getLabelsBatch");
    }

    #[test]
    fn single_words_are_unchanged() {
        assert_eq!(render_js_name("records"), "records");
        assert_eq!(render_js_name("run"), "run");
    }

    #[test]
    fn leading_and_double_underscores_do_not_panic() {
        assert_eq!(render_js_name("_private"), "Private");
        assert_eq!(render_js_name("a__b"), "aB");
    }
}
