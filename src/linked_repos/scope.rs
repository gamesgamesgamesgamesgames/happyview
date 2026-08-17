//! Linked-repo scope handling.
//!
//! The grammar itself lives in `happyview-scopes`, which is pinned to the
//! reference implementation by an interop corpus. What stays here is the part
//! that is HappyView's rather than the protocol's: operator-facing error
//! messages, the advertised scope union, and normalising an HTTP
//! `Content-Type` header before asking a protocol question about it.

pub use happyview_scopes::RepoAction;
use happyview_scopes::{
    AccountPermission, BlobPermission, IdentityPermission, IncludeScope, RepoPermission,
    RpcPermission, ScopePermissions,
};

/// The legacy `transition:` values that mean anything. An authorization server
/// refuses any other, so refusing here turns a dead grant into a message.
const TRANSITION_VALUES: [&str; 3] = ["generic", "chat.bsky", "email"];

pub fn parse(input: &str) -> Vec<String> {
    happyview_scopes::parse_scope_list(input)
}

/// Validate one scope token, returning an operator-facing message on failure.
///
/// The accept/reject decision is the crate's; only the wording is ours. Where a
/// mistake is common and its server-side error unhelpful, the message names the
/// fix rather than restating the rejection.
pub fn validate(scope: &str) -> Result<(), String> {
    if scope.is_empty() {
        return Err("scope must not be empty".into());
    }

    if scope == "atproto" {
        return Ok(());
    }

    let prefix = match scope.split_once([':', '?']) {
        Some((prefix, _)) => prefix,
        None => return Err(format!("unknown scope: {scope}")),
    };

    match prefix {
        "repo" => {
            RepoPermission::parse(scope).map(|_| ()).ok_or_else(|| repo_scope_error(scope))
        }
        "blob" => BlobPermission::parse(scope)
            .map(|_| ())
            .ok_or_else(|| format!("invalid blob scope: {scope} — expected type/subtype, e.g. blob:image/* or blob:*/*")),
        "rpc" => RpcPermission::parse(scope).map(|_| ()).ok_or_else(|| {
            // Overwhelmingly the reason an `rpc:` scope fails: `aud` is
            // required by the grammar and easy to omit.
            if !scope.contains("aud=") {
                format!(
                    "invalid rpc scope: {scope} — rpc scopes require an audience, \
                     e.g. {scope}?aud=* or ?aud=did:web:example.com%23service"
                )
            } else {
                format!(
                    "invalid rpc scope: {scope} — aud must be * or an absolute DID reference \
                     like did:web:example.com%23service, and rpc:*?aud=* is not allowed"
                )
            }
        }),
        "identity" => IdentityPermission::parse(scope)
            .map(|_| ())
            .ok_or_else(|| format!("invalid identity scope: {scope} — expected identity:handle or identity:*")),
        "account" => AccountPermission::parse(scope)
            .map(|_| ())
            .ok_or_else(|| format!("invalid account scope: {scope} — expected account:<email|repo|status> with an optional ?action=read or ?action=manage")),
        "include" => IncludeScope::parse(scope)
            .map(|_| ())
            .ok_or_else(|| format!("invalid include scope: {scope} — expected include:<nsid> with an optional ?aud=<did>")),
        "transition" => match scope.strip_prefix("transition:") {
            Some(value) if TRANSITION_VALUES.contains(&value) => Ok(()),
            Some("") | None => Err("transition scope requires a value".into()),
            Some(other) => Err(format!(
                "unknown transition scope: {other} — expected one of {}",
                TRANSITION_VALUES.join(", ")
            )),
        },
        other => Err(format!("unknown scope prefix: {other}")),
    }
}

/// `repo` gets its own message builder because two of its failure modes are
/// common and neither explains itself server-side.
fn repo_scope_error(scope: &str) -> String {
    if let Some((_, query)) = scope.split_once('?') {
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("action=")
                && value.contains(',')
            {
                let repeated: Vec<String> = value
                    .split(',')
                    .filter(|a| !a.is_empty())
                    .map(|a| format!("action={a}"))
                    .collect();
                return format!(
                    "invalid repo scope: {scope} — repo actions are repeated parameters, \
                     not a comma-separated list; write {}",
                    repeated.join("&")
                );
            }
        }
    }
    format!(
        "invalid repo scope: {scope} — expected repo:<nsid> or repo:* with optional \
         repeated ?action= parameters (create, update, delete)"
    )
}

pub fn client_base_scopes(scope: Option<&str>, client_id: &str) -> String {
    if let Some(s) = scope.map(str::trim).filter(|s| !s.is_empty()) {
        return s.to_string();
    }

    let Some((_, query)) = client_id.split_once('?') else {
        return String::new();
    };
    for pair in query.split('&') {
        let Some(value) = pair.strip_prefix("scope=") else {
            continue;
        };
        let value = value.replace('+', " ");
        return urlencoding::decode(&value)
            .map(|s| s.into_owned())
            .unwrap_or(value);
    }
    String::new()
}

pub fn union(scope_strings: &[String], base: &str) -> String {
    let mut all: Vec<String> = parse(base);
    for grant_scopes in scope_strings {
        for scope in parse(grant_scopes) {
            if !all.contains(&scope) {
                all.push(scope);
            }
        }
    }
    all.sort();
    all.join(" ")
}

/// May this grant write `collection` with `action`?
///
/// Delegates to the shared crate, which is where `transition:generic` is
/// honoured. It previously was not honoured at all here: both matchers stripped
/// a `repo:`/`blob:` prefix and bailed, so a grant holding only the broad legacy
/// scope stored happily and then failed every authorization.
pub fn allows_repo(scopes: &[String], collection: &str, action: RepoAction) -> bool {
    ScopePermissions::from_scopes(scopes.to_vec()).allows_repo(collection, action)
}

/// May this grant upload a blob of this content type?
///
/// Both sides are canonicalised first, which is HTTP's business rather than the
/// scope grammar's:
///
/// - `mime` is a raw `Content-Type` header, so its parameters are stripped
///   (`text/plain; charset=utf-8` → `text/plain`). The crate answers only about
///   a clean MIME type and refuses to match anything that is not one.
/// - `blob:` scope values are lower-cased. The reference matches
///   case-sensitively but *canonicalises* to lower case, so `blob:image/PNG`
///   round-trips through an authorization server as `blob:image/png` and only a
///   hand-written scope would still carry upper case. Folding it here means a
///   grant behaves the same either way.
pub fn allows_blob(scopes: &[String], mime: &str) -> bool {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();

    let canonical: Vec<String> = scopes
        .iter()
        .map(|s| {
            if s.starts_with("blob:") {
                s.to_ascii_lowercase()
            } else {
                s.clone()
            }
        })
        .collect();

    ScopePermissions::from_scopes(canonical).allows_blob(&mime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_and_dedupes() {
        let out = parse("  atproto   repo:com.example.a \n atproto ");
        assert_eq!(out, vec!["atproto", "repo:com.example.a"]);
    }

    #[test]
    fn parse_empty_input_yields_empty() {
        assert!(parse("   \n\t ").is_empty());
    }

    #[test]
    fn validate_accepts_known_shapes() {
        for s in [
            "atproto",
            "identity:*",
            "account:email",
            "repo:*",
            "repo:com.example.note",
            "repo:com.example.note?action=create",
            "repo:com.example.note?action=create&action=update&action=delete",
            "blob:*/*",
            "blob:image/*",
            "blob:image/png",
            "rpc:*?aud=did:web:example.com%23service",
            "rpc:com.example.doThing?aud=*",
            "rpc:com.example.doThing?aud=did:web:example.com%23service",
            "transition:generic",
            "transition:chat.bsky",
            "transition:email",
            "include:com.example.permissionSet",
            "include:com.example.permissionSet?aud=did:web:example.com%23service",
        ] {
            assert!(
                validate(s).is_ok(),
                "expected {s} to validate: {:?}",
                validate(s)
            );
        }
    }

    /// Adopting the shared crate tightened `validate`, which previously waved
    /// through anything non-empty after `rpc:`, `identity:`, `account:`,
    /// `transition:` and `include:`. Every shape below was accepted before and
    /// is refused now — each one an authorization server would have rejected,
    /// so this moves the failure from OAuth time to grant-creation time.
    #[test]
    fn validate_now_rejects_shapes_it_used_to_wave_through() {
        for (scope, expected_hint) in [
            // `aud` is required by the grammar.
            ("rpc:*", "require an audience"),
            ("rpc:com.example.doThing", "require an audience"),
            // An audience must be absolute — a bare DID has no fragment.
            (
                "rpc:com.example.doThing?aud=did:web:example.com",
                "absolute DID reference",
            ),
            // Unbounded "every method against every audience" is forbidden.
            ("rpc:*?aud=*", "not allowed"),
            ("identity:email", "identity:handle"),
            ("account:bogus", "account:<email|repo|status>"),
            ("transition:bogus", "expected one of"),
            ("include:notannsid", "include:<nsid>"),
        ] {
            let err = validate(scope).expect_err(&format!("expected {scope} to be rejected"));
            assert!(
                err.contains(expected_hint),
                "{scope}: expected the message to mention {expected_hint:?}, got: {err}"
            );
        }
    }

    /// The comma-joined form parses as one value `"create,update"`, which is
    /// not an action. An authorization server refuses the whole request with
    /// `invalid_scope`, so a grant spelled this way can never be authorized —
    /// catching it here turns a dead-end grant into an actionable message.
    #[test]
    fn validate_rejects_comma_joined_actions() {
        let err = validate("repo:com.example.note?action=create,update,delete").unwrap_err();
        assert!(
            err.contains("action=create&action=update&action=delete"),
            "error should name the correct spelling, got: {err}"
        );
    }

    #[test]
    fn allows_repo_ignores_comma_joined_actions() {
        let scopes = parse("repo:com.example.note?action=create,update");
        assert!(!allows_repo(
            &scopes,
            "com.example.note",
            RepoAction::Create
        ));
    }

    #[test]
    fn allows_repo_reads_repeated_action_params() {
        let scopes = parse("repo:com.example.note?action=create&action=delete");
        assert!(allows_repo(&scopes, "com.example.note", RepoAction::Create));
        assert!(allows_repo(&scopes, "com.example.note", RepoAction::Delete));
        assert!(!allows_repo(
            &scopes,
            "com.example.note",
            RepoAction::Update
        ));
    }

    #[test]
    fn validate_rejects_malformed() {
        for s in [
            "",
            "repo:",
            "repo:com.example.note?action=",
            "repo:com.example.note?action=frobnicate",
            "blob:image",
            "blob:",
            "nonsense:thing",
            "transition:",
            "include:",
        ] {
            assert!(validate(s).is_err(), "expected {s} to be rejected");
        }
    }

    #[test]
    fn union_merges_dedupes_and_sorts() {
        let grants = vec![
            "repo:com.example.b atproto".to_string(),
            "repo:com.example.a atproto".to_string(),
        ];
        let out = union(&grants, "atproto identity:*");
        assert_eq!(
            out,
            "atproto identity:* repo:com.example.a repo:com.example.b"
        );
    }

    #[test]
    fn union_with_no_grants_is_just_base() {
        assert_eq!(union(&[], "atproto identity:*"), "atproto identity:*");
    }

    #[test]
    fn client_base_scopes_prefers_the_metadata_field() {
        assert_eq!(
            client_base_scopes(Some("atproto identity:*"), "https://example.com/x.json"),
            "atproto identity:*"
        );
    }

    #[test]
    fn client_base_scopes_falls_back_to_a_loopback_client_id() {
        let client_id = "http://localhost?redirect_uri=http%3A%2F%2F127.0.0.1%3A3000%2Fauth%2Fcallback&scope=atproto+identity%3A*";
        assert_eq!(
            client_base_scopes(None, client_id),
            "atproto identity:*",
            "a loopback client's scopes must not vanish from the advertised doc"
        );
    }

    #[test]
    fn client_base_scopes_is_empty_when_there_is_nowhere_to_read_it() {
        assert_eq!(client_base_scopes(None, "http://localhost"), "");
        assert_eq!(client_base_scopes(Some("  "), "http://localhost"), "");
    }

    #[test]
    fn union_carries_every_base_scope_through() {
        let out = union(
            &["repo:com.example.a".to_string()],
            "atproto identity:* account:email",
        );
        assert_eq!(out, "account:email atproto identity:* repo:com.example.a");
    }

    #[test]
    fn allows_repo_defaults_to_all_actions() {
        let scopes = parse("repo:com.example.note");
        assert!(allows_repo(&scopes, "com.example.note", RepoAction::Create));
        assert!(allows_repo(&scopes, "com.example.note", RepoAction::Update));
        assert!(allows_repo(&scopes, "com.example.note", RepoAction::Delete));
    }

    #[test]
    fn allows_repo_honours_action_list() {
        let scopes = parse("repo:com.example.note?action=create&action=update");
        assert!(allows_repo(&scopes, "com.example.note", RepoAction::Create));
        assert!(allows_repo(&scopes, "com.example.note", RepoAction::Update));
        assert!(!allows_repo(
            &scopes,
            "com.example.note",
            RepoAction::Delete
        ));
    }

    #[test]
    fn allows_repo_wildcard_covers_any_collection() {
        let scopes = parse("repo:*");
        assert!(allows_repo(
            &scopes,
            "com.example.anything",
            RepoAction::Delete
        ));
    }

    #[test]
    fn allows_repo_rejects_other_collections() {
        let scopes = parse("repo:com.example.note");
        assert!(!allows_repo(
            &scopes,
            "com.example.other",
            RepoAction::Create
        ));
    }

    #[test]
    fn allows_repo_is_false_without_any_repo_scope() {
        let scopes = parse("atproto identity:*");
        assert!(!allows_repo(
            &scopes,
            "com.example.note",
            RepoAction::Create
        ));
    }

    #[test]
    fn allows_blob_matches_wildcards() {
        assert!(allows_blob(&parse("blob:*/*"), "image/png"));
        assert!(allows_blob(&parse("blob:image/*"), "image/png"));
        assert!(allows_blob(&parse("blob:image/png"), "image/png"));
        assert!(!allows_blob(&parse("blob:image/*"), "video/mp4"));
        assert!(!allows_blob(&parse("blob:image/png"), "image/jpeg"));
        assert!(!allows_blob(&parse("atproto"), "image/png"));
    }

    #[test]
    fn allows_blob_ignores_mime_parameters() {
        assert!(allows_blob(
            &parse("blob:text/plain"),
            "text/plain; charset=utf-8"
        ));
    }

    #[test]
    fn allows_repo_rejects_unrecognized_query() {
        let scopes = parse("repo:com.example.note?foo=bar");
        assert!(!allows_repo(
            &scopes,
            "com.example.note",
            RepoAction::Create
        ));
    }

    #[test]
    fn allows_blob_matches_case_insensitively() {
        assert!(allows_blob(&parse("blob:image/PNG"), "image/png"));
        assert!(allows_blob(&parse("blob:image/png"), "image/PNG"));
    }

    // `transition:generic` is the broad legacy grant. `validate` accepts it, so a
    // grant can be stored holding nothing else — and both matchers used to strip
    // a `repo:`/`blob:` prefix and bail, making it invisible at authorization
    // time. The reference implementation (`ScopePermissionsTransition`) short
    // -circuits `allowsRepo`/`allowsBlob` to true when it is present.
    #[test]
    fn transition_generic_allows_every_repo_action() {
        let scopes = parse("atproto transition:generic");
        for action in [RepoAction::Create, RepoAction::Update, RepoAction::Delete] {
            assert!(
                allows_repo(&scopes, "com.example.note", action),
                "transition:generic should allow {}",
                action.as_str()
            );
        }
    }

    #[test]
    fn transition_generic_allows_any_collection() {
        let scopes = parse("atproto transition:generic");
        assert!(allows_repo(
            &scopes,
            "app.bsky.feed.post",
            RepoAction::Create
        ));
    }

    #[test]
    fn transition_generic_allows_any_blob() {
        let scopes = parse("atproto transition:generic");
        assert!(allows_blob(&scopes, "image/png"));
        assert!(allows_blob(&scopes, "video/mp4"));
    }

    // Only `transition:generic` is broad. `transition:email` and
    // `transition:chat.bsky` are narrow legacy grants that cover neither repo
    // writes nor blob uploads, so they must not open the same door.
    #[test]
    fn other_transition_scopes_do_not_grant_repo_or_blob() {
        let scopes = parse("atproto transition:email transition:chat.bsky");
        assert!(!allows_repo(
            &scopes,
            "com.example.note",
            RepoAction::Create
        ));
        assert!(!allows_blob(&scopes, "image/png"));
    }

    #[test]
    fn validate_repo_rejects_malformed_nsid() {
        for s in ["repo:not an nsid", "repo:com"] {
            assert!(validate(s).is_err(), "expected {s} to be rejected");
        }
    }

    #[test]
    fn validate_repo_accepts_wildcard_and_valid_nsid() {
        assert!(validate("repo:*").is_ok());
        assert!(validate("repo:com.example.note").is_ok());
    }
}
