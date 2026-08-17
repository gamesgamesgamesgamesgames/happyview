//! AT Protocol OAuth scope parsing and matching.
//!
//! Ported from `@atproto/oauth-scopes` (v0.3.1), which is the normative source:
//! proposal 0011 defines the grammar but states outright that it "does not
//! include full definitions for the initial permissions," deferring those to
//! the reference implementation. Correctness here is pinned by the interop
//! corpus in `tests/`, generated from that package — not by trusting this port.
//!
//! # What this crate does and does not answer
//!
//! It answers *semantic* questions — may this collection be created, may this
//! lexicon method be called against this audience, may this MIME type be
//! uploaded. It deliberately does **not** map XRPC method names to those
//! questions: that mapping lives inside the PDS, is not specified anywhere, and
//! is still evolving. A caller that needs it must supply it, and should only do
//! so for operations it can map unambiguously.
//!
//! # Why this is a crate
//!
//! HappyView previously had two independent scope implementations that shared
//! no code and disagreed with the reference in five separate ways, including
//! one that made `transition:generic` invisible to linked-repo authorization
//! and one that omitted the permission-set authority-containment check. One
//! implementation, pinned to a corpus, is the fix.

pub mod include;
pub mod resources;
pub mod syntax;

pub use include::{IncludeScope, IncludedPermission, LexPermission, LexPermissionSet, LexValue};
pub use resources::{
    ACCOUNT_ATTRIBUTES, AccountAction, AccountPermission, BlobPermission, IDENTITY_ATTRIBUTES,
    IdentityPermission, RepoAction, RepoPermission, RpcPermission, accept_covers,
    is_absolute_did_ref, is_accept, is_mime,
};
pub use syntax::ScopeSyntax;

/// Split a scope string on whitespace, preserving order and dropping
/// duplicates.
pub fn parse_scope_list(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in input.split_whitespace() {
        if !out.iter().any(|s| s == token) {
            out.push(token.to_string());
        }
    }
    out
}

/// A granted scope set, answering permission questions against it.
///
/// Legacy `transition:*` scopes are honoured here rather than in the individual
/// permission types, mirroring the reference's `ScopePermissionsTransition`.
#[derive(Debug, Clone, Default)]
pub struct ScopePermissions {
    scopes: Vec<String>,
}

impl ScopePermissions {
    /// Build from a whitespace-separated scope string.
    pub fn parse(scope: &str) -> Self {
        Self {
            scopes: parse_scope_list(scope),
        }
    }

    /// Build from an already-split scope list.
    pub fn from_scopes(scopes: impl IntoIterator<Item = String>) -> Self {
        Self {
            scopes: scopes.into_iter().collect(),
        }
    }

    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }

    pub fn has(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    /// The broad legacy grant. Covers `repo`, `blob`, and every `rpc` whose
    /// method is not under `chat.bsky.` — but **not** `identity` or `account`,
    /// which is why `com.atproto.identity.updateHandle` stays gated even for a
    /// client holding it.
    pub fn has_transition_generic(&self) -> bool {
        self.has("transition:generic")
    }

    pub fn has_transition_email(&self) -> bool {
        self.has("transition:email")
    }

    pub fn has_transition_chat_bsky(&self) -> bool {
        self.has("transition:chat.bsky")
    }

    pub fn allows_repo(&self, collection: &str, action: RepoAction) -> bool {
        if self.has_transition_generic() {
            return true;
        }
        self.scopes
            .iter()
            .filter_map(|s| RepoPermission::parse(s))
            .any(|p| p.matches(collection, action))
    }

    pub fn allows_rpc(&self, lxm: &str, aud: &str) -> bool {
        if self.has_transition_generic() && !lxm.starts_with("chat.bsky.") {
            return true;
        }
        if self.has_transition_chat_bsky() && lxm.starts_with("chat.bsky.") {
            return true;
        }
        self.scopes
            .iter()
            .filter_map(|s| RpcPermission::parse(s))
            .any(|p| p.matches(lxm, aud))
    }

    pub fn allows_blob(&self, mime: &str) -> bool {
        if self.has_transition_generic() {
            return true;
        }
        self.scopes
            .iter()
            .filter_map(|s| BlobPermission::parse(s))
            .any(|p| p.matches(mime))
    }

    pub fn allows_identity(&self, attr: &str) -> bool {
        self.scopes
            .iter()
            .filter_map(|s| IdentityPermission::parse(s))
            .any(|p| p.matches(attr))
    }

    /// Is an accept pattern covered? Unlike [`Self::allows_blob`], which asks
    /// about a concrete upload, this asks whether a *grant* of `accept` would
    /// stay within what is already held — needed for subset checks, where the
    /// thing being tested is itself a pattern.
    pub fn allows_accept(&self, accept: &str) -> bool {
        if self.has_transition_generic() {
            return true;
        }
        self.scopes
            .iter()
            .filter_map(|s| BlobPermission::parse(s))
            .any(|p| p.accept.iter().any(|held| accept_covers(held, accept)))
    }

    /// Does this scope set fully contain everything `scope` would grant?
    ///
    /// Used to check a token's scopes against what a client is registered for.
    /// A permission is covered only when **every** grant it makes is covered —
    /// a token asking for create, update and delete is not satisfied by a
    /// client registered for create alone.
    ///
    /// A wildcard in `scope` asks for the wildcard specifically: the matchers
    /// compare `*` literally, so only a held `*` answers it.
    pub fn covers_scope(&self, scope: &str) -> bool {
        if scope == "atproto" || self.has(scope) {
            return true;
        }

        let prefix = match scope.split_once([':', '?']) {
            Some((prefix, _)) => prefix,
            // Not a resource scope and not held verbatim.
            None => return false,
        };

        match prefix {
            "repo" => RepoPermission::parse(scope).is_some_and(|p| {
                p.collection.iter().all(|collection| {
                    p.action
                        .iter()
                        .all(|action| self.allows_repo(collection, *action))
                })
            }),
            "rpc" => RpcPermission::parse(scope)
                .is_some_and(|p| p.lxm.iter().all(|lxm| self.allows_rpc(lxm, &p.aud))),
            "blob" => BlobPermission::parse(scope)
                .is_some_and(|p| p.accept.iter().all(|a| self.allows_accept(a))),
            "identity" => {
                IdentityPermission::parse(scope).is_some_and(|p| self.allows_identity(&p.attr))
            }
            "account" => AccountPermission::parse(scope).is_some_and(|p| {
                p.action
                    .iter()
                    .all(|action| self.allows_account(&p.attr, *action))
            }),
            // `transition:` and `include:` are not permissions and are covered
            // only by holding the identical scope, handled above.
            _ => false,
        }
    }

    pub fn allows_account(&self, attr: &str, action: AccountAction) -> bool {
        if self.has_transition_email() && attr == "email" && action == AccountAction::Read {
            return true;
        }
        self.scopes
            .iter()
            .filter_map(|s| AccountPermission::parse(s))
            .any(|p| p.matches(attr, action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scope_list_dedupes_and_preserves_order() {
        assert_eq!(
            parse_scope_list("  atproto  repo:com.example.a \n atproto "),
            vec!["atproto", "repo:com.example.a"]
        );
    }

    #[test]
    fn transition_generic_covers_repo_and_blob() {
        let p = ScopePermissions::parse("atproto transition:generic");
        assert!(p.allows_repo("com.example.post", RepoAction::Create));
        assert!(p.allows_repo("app.bsky.feed.post", RepoAction::Delete));
        assert!(p.allows_blob("image/png"));
    }

    #[test]
    fn transition_generic_covers_rpc_except_chat_bsky() {
        let p = ScopePermissions::parse("atproto transition:generic");
        assert!(p.allows_rpc("com.example.getFeed", "did:web:x.com#svc"));
        assert!(!p.allows_rpc("chat.bsky.convo.listConvos", "did:web:x.com#svc"));
    }

    #[test]
    fn transition_chat_bsky_covers_only_chat() {
        let p = ScopePermissions::parse("atproto transition:chat.bsky");
        assert!(p.allows_rpc("chat.bsky.convo.listConvos", "did:web:x.com#svc"));
        assert!(!p.allows_rpc("com.example.getFeed", "did:web:x.com#svc"));
    }

    /// The distinction that keeps identity and account operations gated for
    /// clients holding only the broad legacy grant.
    #[test]
    fn transition_generic_does_not_cover_identity_or_account() {
        let p = ScopePermissions::parse("atproto transition:generic");
        assert!(!p.allows_identity("handle"));
        assert!(!p.allows_account("email", AccountAction::Read));
        assert!(!p.allows_account("repo", AccountAction::Manage));
    }

    #[test]
    fn transition_email_covers_only_reading_email() {
        let p = ScopePermissions::parse("atproto transition:email");
        assert!(p.allows_account("email", AccountAction::Read));
        assert!(!p.allows_account("email", AccountAction::Manage));
        assert!(!p.allows_account("status", AccountAction::Read));
    }

    #[test]
    fn granular_scopes_match_without_any_transition() {
        let p = ScopePermissions::parse(
            "atproto repo:com.example.post?action=create identity:handle account:email",
        );
        assert!(p.allows_repo("com.example.post", RepoAction::Create));
        assert!(!p.allows_repo("com.example.post", RepoAction::Delete));
        assert!(!p.allows_repo("com.other.post", RepoAction::Create));
        assert!(p.allows_identity("handle"));
        assert!(p.allows_account("email", AccountAction::Read));
    }

    #[test]
    fn malformed_scopes_are_ignored_not_fatal() {
        // A bare `rpc:` (no aud) does not parse, so it grants nothing — but it
        // must not stop the rest of the set from being honoured.
        let p = ScopePermissions::parse("atproto rpc:com.example.a repo:com.example.post");
        assert!(!p.allows_rpc("com.example.a", "did:web:x.com#svc"));
        assert!(p.allows_repo("com.example.post", RepoAction::Create));
    }
}
