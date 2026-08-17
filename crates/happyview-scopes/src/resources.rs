//! The five protocol resource permissions: `repo`, `rpc`, `blob`, `identity`,
//! `account`.
//!
//! Each type parses from a scope string and answers a *semantic* question —
//! "may this collection be created", "may this lexicon method be called against
//! this audience" — rather than "is this XRPC method allowed". That distinction
//! is the reference implementation's, and it is load-bearing: the mapping from
//! an XRPC method to one of these questions lives in the PDS and is not part of
//! the scope grammar.

use crate::syntax::ScopeSyntax;

// ---------------------------------------------------------------------------
// Shared parser rules
// ---------------------------------------------------------------------------

/// The reference rejects a scope carrying any parameter its schema does not
/// declare, rather than ignoring the stray key.
fn has_only_known_keys(syntax: &ScopeSyntax, known: &[&str]) -> bool {
    syntax.keys().iter().all(|k| known.contains(k))
}

/// A positional value and a named parameter for the same field cannot both be
/// present.
fn positional_conflicts(syntax: &ScopeSyntax, name: &str) -> bool {
    syntax.positional.is_some() && syntax.get_multi(name).is_some()
}

/// Resolve a `multiple: true` field that may also be given positionally.
fn multi_or_positional(syntax: &ScopeSyntax, name: &str) -> Option<Vec<String>> {
    if let Some(values) = syntax.get_multi(name) {
        if values.is_empty() {
            return None;
        }
        return Some(values.into_iter().map(str::to_string).collect());
    }
    syntax.positional.as_ref().map(|p| vec![p.clone()])
}

/// Resolve a `multiple: false` field that may also be given positionally.
/// The outer `None` means the parameter repeated, which invalidates the scope.
fn single_or_positional(syntax: &ScopeSyntax, name: &str) -> Option<Option<String>> {
    match syntax.get_single(name).ok()? {
        Some(v) => Some(Some(v.to_string())),
        None => Some(syntax.positional.clone()),
    }
}

fn is_nsid(value: &str) -> bool {
    happyview_nsid::validate_nsid(value).is_ok()
}

/// `*` or a valid NSID — the shape shared by `repo`'s `collection` and `rpc`'s
/// `lxm`.
fn is_nsid_or_wildcard(value: &str) -> bool {
    value == "*" || is_nsid(value)
}

// ---------------------------------------------------------------------------
// DID references (the `aud` parameter)
// ---------------------------------------------------------------------------

/// An absolute atproto DID reference: a supported DID followed by exactly one
/// non-empty `#fragment`.
///
/// Validation is method-specific and matches `@atproto/did`, pinned against it:
/// `did:plc:` takes exactly 24 base32-lower characters, `did:web:` takes a
/// hostname with no port and no path segments, and no other method is accepted.
pub fn is_absolute_did_ref(value: &str) -> bool {
    let Some((did, fragment)) = value.split_once('#') else {
        return false;
    };
    if fragment.is_empty() || fragment.contains('#') {
        return false;
    }
    is_supported_did(did)
}

fn is_supported_did(did: &str) -> bool {
    if let Some(id) = did.strip_prefix("did:plc:") {
        // Base32-lower ("a"-"z", "2"-"7"), exactly 24 characters.
        return id.len() == 24
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b));
    }
    if let Some(host) = did.strip_prefix("did:web:") {
        return is_hostname(host);
    }
    false
}

/// A bare hostname: dot-separated labels of alphanumerics and hyphens, no empty
/// labels, and no port or path. Case is *not* normalised — the reference
/// accepts `did:web:Example.com`.
fn is_hostname(host: &str) -> bool {
    if host.is_empty() {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

// ---------------------------------------------------------------------------
// repo
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoAction {
    Create,
    Update,
    Delete,
}

impl RepoAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "create" => Some(Self::Create),
            "update" => Some(Self::Update),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    pub const ALL: [RepoAction; 3] = [Self::Create, Self::Update, Self::Delete];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPermission {
    pub collection: Vec<String>,
    pub action: Vec<RepoAction>,
}

impl RepoPermission {
    pub fn parse(scope: &str) -> Option<Self> {
        let syntax = ScopeSyntax::parse(scope);
        if syntax.prefix != "repo" {
            return None;
        }
        if !has_only_known_keys(&syntax, &["collection", "action"]) {
            return None;
        }
        if positional_conflicts(&syntax, "collection") {
            return None;
        }

        let collection = multi_or_positional(&syntax, "collection")?;
        let action = syntax
            .get_multi("action")
            .map(|v| v.into_iter().map(str::to_string).collect::<Vec<_>>());

        Self::from_parts(collection, action)
    }

    /// Validate and construct from already-extracted parts. Shared by the scope
    /// -string path and the lexicon permission-set path, so the two cannot
    /// disagree about what a valid `repo` permission is.
    pub fn from_parts(collection: Vec<String>, action: Option<Vec<String>>) -> Option<Self> {
        if collection.is_empty() || !collection.iter().all(|c| is_nsid_or_wildcard(c)) {
            return None;
        }

        let action = match action {
            Some(values) => {
                if values.is_empty() {
                    return None;
                }
                values
                    .iter()
                    .map(|v| RepoAction::parse(v))
                    .collect::<Option<Vec<_>>>()?
            }
            None => RepoAction::ALL.to_vec(),
        };

        Some(Self { collection, action })
    }

    pub fn matches(&self, collection: &str, action: RepoAction) -> bool {
        self.action.contains(&action)
            && (self.collection.iter().any(|c| c == "*")
                || self.collection.iter().any(|c| c == collection))
    }
}

// ---------------------------------------------------------------------------
// rpc
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcPermission {
    /// `*` or an absolute DID reference. Required — an `rpc:` scope with no
    /// audience does not parse.
    pub aud: String,
    pub lxm: Vec<String>,
}

impl RpcPermission {
    pub fn parse(scope: &str) -> Option<Self> {
        let syntax = ScopeSyntax::parse(scope);
        if syntax.prefix != "rpc" {
            return None;
        }
        if !has_only_known_keys(&syntax, &["lxm", "aud"]) {
            return None;
        }
        if positional_conflicts(&syntax, "lxm") {
            return None;
        }

        let lxm = multi_or_positional(&syntax, "lxm")?;
        let aud = syntax.get_single("aud").ok()?.map(str::to_string);

        Self::from_parts(lxm, aud)
    }

    /// Validate and construct from already-extracted parts. Shared by the scope
    /// -string path and the lexicon permission-set path.
    pub fn from_parts(lxm: Vec<String>, aud: Option<String>) -> Option<Self> {
        if lxm.is_empty() || !lxm.iter().all(|l| is_nsid_or_wildcard(l)) {
            return None;
        }

        // `aud` is required: an `rpc:` scope without an audience does not parse.
        let aud = aud?;
        if aud != "*" && !is_absolute_did_ref(&aud) {
            return None;
        }

        // `rpc:*?aud=*` is forbidden outright — an unbounded grant of every
        // method against every audience is not expressible. Either the method
        // set or the audience must be pinned. This is a special case in the
        // reference rather than a consequence of the grammar, and the interop
        // corpus is what caught its absence here.
        if aud == "*" && lxm.iter().any(|l| l == "*") {
            return None;
        }

        Some(Self { aud, lxm })
    }

    pub fn matches(&self, lxm: &str, aud: &str) -> bool {
        (self.aud == "*" || self.aud == aud)
            && (self.lxm.iter().any(|l| l == "*") || self.lxm.iter().any(|l| l == lxm))
    }
}

// ---------------------------------------------------------------------------
// blob
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobPermission {
    pub accept: Vec<String>,
}

/// `type/subtype` with exactly one slash, both halves non-empty, no spaces.
fn is_type_slash_subtype(value: &str) -> bool {
    let Some(slash) = value.find('/') else {
        return false;
    };
    slash != 0
        && slash != value.len() - 1
        && !value[slash + 1..].contains('/')
        && !value.contains(' ')
}

/// A concrete MIME type: `type/subtype` with no wildcard anywhere.
pub fn is_mime(value: &str) -> bool {
    is_type_slash_subtype(value) && !value.contains('*')
}

/// An accept pattern: `*/*`, `type/*`, or a concrete MIME type.
pub fn is_accept(value: &str) -> bool {
    if value == "*/*" {
        return true;
    }
    if !is_type_slash_subtype(value) {
        return false;
    }
    !value.contains('*') || value.ends_with("/*")
}

/// Does the accept pattern `held` subsume the accept pattern `wanted`?
///
/// Pattern-vs-pattern, unlike [`accept_matches`], which is pattern-vs-concrete.
/// `image/*` subsumes `image/png` but not the other way round, and nothing
/// except `*/*` subsumes `*/*`.
pub fn accept_covers(held: &str, wanted: &str) -> bool {
    if held == "*/*" {
        return true;
    }
    if wanted == "*/*" {
        return false;
    }
    if let Some(prefix) = held.strip_suffix('*') {
        return wanted.starts_with(prefix);
    }
    held == wanted
}

fn accept_matches(accept: &str, mime: &str) -> bool {
    if accept == "*/*" {
        return true;
    }
    if let Some(prefix) = accept.strip_suffix('*') {
        return mime.starts_with(prefix);
    }
    accept == mime
}

impl BlobPermission {
    pub fn parse(scope: &str) -> Option<Self> {
        let syntax = ScopeSyntax::parse(scope);
        if syntax.prefix != "blob" {
            return None;
        }
        if !has_only_known_keys(&syntax, &["accept"]) {
            return None;
        }
        if positional_conflicts(&syntax, "accept") {
            return None;
        }

        let accept = multi_or_positional(&syntax, "accept")?;
        if !accept.iter().all(|a| is_accept(a)) {
            return None;
        }

        Some(Self { accept })
    }

    /// The queried value must itself be a concrete MIME type — asking whether
    /// `image/*` is permitted is not a question this answers.
    pub fn matches(&self, mime: &str) -> bool {
        is_mime(mime) && self.accept.iter().any(|a| accept_matches(a, mime))
    }
}

// ---------------------------------------------------------------------------
// identity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityPermission {
    pub attr: String,
}

pub const IDENTITY_ATTRIBUTES: [&str; 2] = ["handle", "*"];

impl IdentityPermission {
    pub fn parse(scope: &str) -> Option<Self> {
        let syntax = ScopeSyntax::parse(scope);
        if syntax.prefix != "identity" {
            return None;
        }
        if !has_only_known_keys(&syntax, &["attr"]) {
            return None;
        }
        if positional_conflicts(&syntax, "attr") {
            return None;
        }

        let attr = single_or_positional(&syntax, "attr")??;
        if !IDENTITY_ATTRIBUTES.contains(&attr.as_str()) {
            return None;
        }

        Some(Self { attr })
    }

    pub fn matches(&self, attr: &str) -> bool {
        self.attr == "*" || self.attr == attr
    }
}

// ---------------------------------------------------------------------------
// account
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAction {
    Read,
    Manage,
}

impl AccountAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Manage => "manage",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "read" => Some(Self::Read),
            "manage" => Some(Self::Manage),
            _ => None,
        }
    }
}

pub const ACCOUNT_ATTRIBUTES: [&str; 3] = ["email", "repo", "status"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPermission {
    pub attr: String,
    pub action: Vec<AccountAction>,
}

impl AccountPermission {
    pub fn parse(scope: &str) -> Option<Self> {
        let syntax = ScopeSyntax::parse(scope);
        if syntax.prefix != "account" {
            return None;
        }
        if !has_only_known_keys(&syntax, &["attr", "action"]) {
            return None;
        }
        if positional_conflicts(&syntax, "attr") {
            return None;
        }

        let attr = single_or_positional(&syntax, "attr")??;
        if !ACCOUNT_ATTRIBUTES.contains(&attr.as_str()) {
            return None;
        }

        let action = match syntax.get_multi("action") {
            Some(values) => {
                if values.is_empty() {
                    return None;
                }
                values
                    .into_iter()
                    .map(AccountAction::parse)
                    .collect::<Option<Vec<_>>>()?
            }
            None => vec![AccountAction::Read],
        };

        Some(Self { attr, action })
    }

    /// `manage` subsumes `read`.
    pub fn matches(&self, attr: &str, action: AccountAction) -> bool {
        self.attr == attr
            && (self.action.contains(&AccountAction::Manage) || self.action.contains(&action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_positional_defaults_to_all_actions() {
        let p = RepoPermission::parse("repo:com.example.post").unwrap();
        assert_eq!(p.collection, vec!["com.example.post"]);
        assert_eq!(p.action, RepoAction::ALL.to_vec());
    }

    #[test]
    fn repo_rejects_comma_joined_actions() {
        assert!(RepoPermission::parse("repo:com.example.post?action=create,delete").is_none());
    }

    #[test]
    fn repo_accepts_repeated_action_params() {
        let p = RepoPermission::parse("repo:com.example.post?action=create&action=delete").unwrap();
        assert_eq!(p.action, vec![RepoAction::Create, RepoAction::Delete]);
    }

    #[test]
    fn repo_rejects_unknown_param() {
        assert!(RepoPermission::parse("repo:com.example.post?action=create&foo=bar").is_none());
    }

    #[test]
    fn repo_rejects_positional_and_named_collection_together() {
        assert!(RepoPermission::parse("repo:com.example.post?collection=com.other.x").is_none());
    }

    #[test]
    fn repo_named_collection_form() {
        let p = RepoPermission::parse("repo?collection=com.example.post&action=delete").unwrap();
        assert!(p.matches("com.example.post", RepoAction::Delete));
        assert!(!p.matches("com.example.post", RepoAction::Create));
    }

    #[test]
    fn repo_wildcard_collection() {
        let p = RepoPermission::parse("repo:*").unwrap();
        assert!(p.matches("anything.at.all", RepoAction::Update));
    }

    #[test]
    fn rpc_requires_an_aud() {
        assert!(RpcPermission::parse("rpc:com.example.getFeed").is_none());
        assert!(RpcPermission::parse("rpc:com.example.getFeed?aud=*").is_some());
    }

    #[test]
    fn rpc_aud_did_validation_is_method_specific() {
        let ok =
            |aud: &str| RpcPermission::parse(&format!("rpc:com.example.a?aud={aud}")).is_some();
        assert!(ok("did:plc:6msi3pj7krzih5qxqtryxlzw%23atproto_pds"));
        assert!(ok("did:web:api.bsky.app%23bsky_appview"));
        assert!(ok("did:web:localhost%23svc"));
        // 23 and 25 characters, uppercase, and out-of-alphabet digits all fail.
        assert!(!ok("did:plc:6msi3pj7krzih5qxqtryxlz%23s"));
        assert!(!ok("did:plc:6msi3pj7krzih5qxqtryxlzwz%23s"));
        assert!(!ok("did:plc:6MSI3PJ7KRZIH5QXQTRYXLZW%23s"));
        assert!(!ok("did:plc:0189i3pj7krzih5qxqtryxlz%23s"));
        // No fragment, empty fragment, doubled fragment, unsupported method,
        // ports and path segments.
        assert!(!ok("did:web:example.com"));
        assert!(!ok("did:web:example.com%23"));
        assert!(!ok("did:web:example.com%23a%23b"));
        assert!(!ok("did:foo:bar%23svc"));
        assert!(!ok("did:web:example.com%3A3000%23s"));
        assert!(!ok("did:web:example.com:user:alice%23s"));
    }

    #[test]
    fn rpc_matches_wildcards() {
        // A wildcard method set is allowed only against a pinned audience.
        let p = RpcPermission::parse("rpc:*?aud=did:web:x.com%23s").unwrap();
        assert!(p.matches("com.example.anything", "did:web:x.com#s"));
        assert!(!p.matches("com.example.anything", "did:web:other.com#s"));

        // And a wildcard audience only against a pinned method set.
        let p = RpcPermission::parse("rpc:com.example.a?aud=*").unwrap();
        assert!(p.matches("com.example.a", "did:web:anything.com#s"));
        assert!(!p.matches("com.example.b", "did:web:anything.com#s"));
    }

    #[test]
    fn rpc_refuses_wildcard_method_and_wildcard_audience_together() {
        assert!(RpcPermission::parse("rpc:*?aud=*").is_none());
        assert!(RpcPermission::parse("rpc?lxm=*&aud=*").is_none());
        // Still refused when the wildcard is one of several methods.
        assert!(RpcPermission::parse("rpc?lxm=com.example.a&lxm=*&aud=*").is_none());
    }

    #[test]
    fn blob_accept_matching() {
        assert!(
            BlobPermission::parse("blob:*/*")
                .unwrap()
                .matches("image/png")
        );
        assert!(
            BlobPermission::parse("blob:image/*")
                .unwrap()
                .matches("image/png")
        );
        assert!(
            !BlobPermission::parse("blob:image/*")
                .unwrap()
                .matches("video/mp4")
        );
        assert!(BlobPermission::parse("blob:image").is_none());
    }

    #[test]
    fn blob_query_must_be_a_concrete_mime() {
        // A wildcard is a valid *grant* but not a valid *question*.
        assert!(
            !BlobPermission::parse("blob:*/*")
                .unwrap()
                .matches("image/*")
        );
    }

    #[test]
    fn identity_known_attributes_only() {
        assert!(IdentityPermission::parse("identity:handle").is_some());
        assert!(IdentityPermission::parse("identity:*").is_some());
        assert!(IdentityPermission::parse("identity:email").is_none());
    }

    #[test]
    fn account_defaults_to_read_and_manage_subsumes_it() {
        let p = AccountPermission::parse("account:email").unwrap();
        assert!(p.matches("email", AccountAction::Read));
        assert!(!p.matches("email", AccountAction::Manage));

        let p = AccountPermission::parse("account:email?action=manage").unwrap();
        assert!(p.matches("email", AccountAction::Manage));
        assert!(p.matches("email", AccountAction::Read));
    }

    #[test]
    fn account_rejects_unknown_attribute() {
        assert!(AccountPermission::parse("account:bogus").is_none());
    }
}
