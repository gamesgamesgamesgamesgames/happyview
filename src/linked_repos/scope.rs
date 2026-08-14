#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoAction {
    Create,
    Update,
    Delete,
}

impl RepoAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

pub fn parse(input: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for token in input.split_whitespace() {
        let token = token.to_string();
        if !seen.contains(&token) {
            seen.push(token);
        }
    }
    seen
}

pub fn validate(scope: &str) -> Result<(), String> {
    if scope.is_empty() {
        return Err("scope must not be empty".into());
    }

    if scope == "atproto" {
        return Ok(());
    }

    let (prefix, rest) = match scope.split_once(':') {
        Some(parts) => parts,
        None => return Err(format!("unknown scope: {scope}")),
    };

    match prefix {
        "repo" => validate_repo(rest),
        "blob" => validate_blob(rest),
        "rpc" | "identity" | "account" | "transition" | "include" => {
            if rest.is_empty() {
                Err(format!("{prefix} scope requires a value"))
            } else {
                Ok(())
            }
        }
        other => Err(format!("unknown scope prefix: {other}")),
    }
}

fn validate_repo(rest: &str) -> Result<(), String> {
    let (target, query) = split_query(rest);
    if target.is_empty() {
        return Err("repo scope requires a collection or *".into());
    }
    if target != "*" {
        happyview_nsid::validate_nsid(target).map_err(|_| format!("invalid NSID: {target}"))?;
    }
    let Some(query) = query else { return Ok(()) };

    let Some(actions) = query.strip_prefix("action=") else {
        return Err(format!("unsupported repo scope parameter: {query}"));
    };
    if actions.is_empty() {
        return Err("repo scope action list must not be empty".into());
    }
    for action in actions.split(',') {
        if !matches!(action, "create" | "update" | "delete") {
            return Err(format!("unknown repo action: {action}"));
        }
    }
    Ok(())
}

fn validate_blob(rest: &str) -> Result<(), String> {
    let (target, _) = split_query(rest);
    let Some((ty, subtype)) = target.split_once('/') else {
        return Err("blob scope must be type/subtype".into());
    };
    if ty.is_empty() || subtype.is_empty() {
        return Err("blob scope must be type/subtype".into());
    }
    Ok(())
}

fn split_query(rest: &str) -> (&str, Option<&str>) {
    match rest.split_once('?') {
        Some((target, query)) => (target, Some(query)),
        None => (rest, None),
    }
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

pub fn allows_repo(scopes: &[String], collection: &str, action: RepoAction) -> bool {
    scopes.iter().any(|scope| {
        let Some(rest) = scope.strip_prefix("repo:") else {
            return false;
        };
        let (target, query) = split_query(rest);

        if target != "*" && target != collection {
            return false;
        }

        match query {
            // No query at all means all actions.
            None => true,
            Some(q) => match q.strip_prefix("action=") {
                Some(actions) => actions.split(',').any(|a| a == action.as_str()),
                None => false,
            },
        }
    })
}

pub fn allows_blob(scopes: &[String], mime: &str) -> bool {
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    let Some((mime_ty, mime_sub)) = mime.split_once('/') else {
        return false;
    };

    let mime_ty = mime_ty.to_ascii_lowercase();
    let mime_sub = mime_sub.to_ascii_lowercase();

    scopes.iter().any(|scope| {
        let Some(rest) = scope.strip_prefix("blob:") else {
            return false;
        };
        let (target, _) = split_query(rest);
        let Some((ty, sub)) = target.split_once('/') else {
            return false;
        };
        let ty = ty.to_ascii_lowercase();
        let sub = sub.to_ascii_lowercase();
        (ty == "*" || ty == mime_ty) && (sub == "*" || sub == mime_sub)
    })
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
            "repo:com.example.note?action=create,update,delete",
            "blob:*/*",
            "blob:image/*",
            "blob:image/png",
            "rpc:*",
            "rpc:com.example.doThing?aud=did:web:example.com",
            "transition:generic",
            "transition:chat.bsky",
            "include:com.example.permissionSet",
        ] {
            assert!(validate(s).is_ok(), "expected {s} to validate");
        }
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
        let scopes = parse("repo:com.example.note?action=create,update");
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
