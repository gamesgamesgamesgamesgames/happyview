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

// ---------------------------------------------------------------------------
// space
// ---------------------------------------------------------------------------

/// What a `space:` grant permits on the **records** in the spaces it selects.
///
/// Declaration order is the reference's `SPACE_ACTIONS`, which is also the
/// normalisation order: parsed values are re-emitted in this sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpaceAction {
    /// The holder's own repo only, and **not** `getDelegationToken`.
    ReadSelf,
    /// Whole-space read, plus `getDelegationToken`.
    Read,
    Create,
    Update,
    Delete,
}

impl SpaceAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadSelf => "read_self",
            Self::Read => "read",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "read_self" => Some(Self::ReadSelf),
            "read" => Some(Self::Read),
            "create" => Some(Self::Create),
            "update" => Some(Self::Update),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    pub const ALL: [SpaceAction; 5] = [
        Self::ReadSelf,
        Self::Read,
        Self::Create,
        Self::Update,
        Self::Delete,
    ];

    /// The default `action` set. `read_self` is omitted because `read` implies it.
    pub const DEFAULT: [SpaceAction; 4] = [Self::Read, Self::Create, Self::Update, Self::Delete];
}

/// What a `space:` grant permits on the **spaces themselves**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ManageOp {
    Create,
    Update,
    Delete,
}

impl ManageOp {
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

    pub const ALL: [ManageOp; 3] = [Self::Create, Self::Update, Self::Delete];
}

/// The operation a `space:` grant is being asked about.
///
/// Record actions and management ops are mutually exclusive, and the reference
/// models them as one tagged target. `collection` is meaningful only for the
/// three record writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceTarget<'a> {
    /// Whole-space read. Collection-independent.
    Read,
    /// Own-repo read. Collection-independent.
    ReadSelf,
    /// A record write against a specific collection.
    Write {
        action: SpaceAction,
        collection: &'a str,
    },
    /// An operation on the space itself.
    Manage(ManageOp),
}

/// Any syntactically valid DID: `did:<method>:<id>`.
///
/// Broader than [`is_absolute_did_ref`], which is the `rpc` `aud`
/// shape. The reference validates a space authority with `isValidDid`, which
/// accepts any method rather than the two atproto ships today.
fn is_valid_did(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("did:") else {
        return false;
    };
    let Some((method, id)) = rest.split_once(':') else {
        return false;
    };
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_lowercase()) {
        return false;
    }
    if id.is_empty() || id.ends_with(':') {
        return false;
    }
    id.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':' | b'%'))
}

/// A space key has the same syntax as a record key.
fn is_valid_record_key(value: &str) -> bool {
    if value.is_empty() || value.len() > 512 || value == "." || value == ".." {
        return false;
    }
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':' | b'~'))
}

/// `space:<spaceType>[?authority=][&skey=][&collection=][&action=][&manage=]`
///
/// `authority`, `space_type` and `skey` select *which spaces* the grant covers;
/// `action` (with `collection`) governs their records; `manage` governs the
/// spaces themselves.
///
/// The matcher is **context-free**, matching the reference: an `authority` of
/// `self` and an empty `collection` list are resolved at token-issuance time via
/// [`with_resolved_authority`](Self::with_resolved_authority) and
/// [`with_default_collections`](Self::with_default_collections), not looked up
/// during a permission check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpacePermission {
    /// A space-type NSID, or `*`.
    pub space_type: String,
    /// A DID, `self` (the granting user), or `*`. Defaults to `self`.
    pub authority: String,
    /// A space key, or `*`. Defaults to `*`.
    pub skey: String,
    /// Empty means **no write targets**, not "all collections", so a bare grant
    /// cannot write every collection in the space. Issuance materialises the
    /// space type's declared set into a bare grant.
    pub collection: Vec<String>,
    pub action: Vec<SpaceAction>,
    pub manage: Vec<ManageOp>,
}

impl SpacePermission {
    pub fn parse(scope: &str) -> Option<Self> {
        let syntax = ScopeSyntax::parse(scope);
        if syntax.prefix != "space" {
            return None;
        }
        if !has_only_known_keys(
            &syntax,
            &["authority", "skey", "collection", "action", "manage"],
        ) {
            return None;
        }
        if positional_conflicts(&syntax, "type") {
            return None;
        }

        let space_type = syntax.positional.clone()?;
        let multi = |key: &str| {
            syntax
                .get_multi(key)
                .map(|v| v.into_iter().map(str::to_string).collect::<Vec<_>>())
        };

        Self::from_parts(
            space_type,
            syntax.get_single("authority").ok()?.map(str::to_string),
            syntax.get_single("skey").ok()?.map(str::to_string),
            multi("collection"),
            multi("action"),
            multi("manage"),
        )
    }

    /// Validate and construct from already-extracted parts.
    ///
    /// Shared by the scope-string path and the lexicon permission-set path, so
    /// the two cannot disagree about what a valid `space` permission is.
    pub fn from_parts(
        space_type: String,
        authority: Option<String>,
        skey: Option<String>,
        collection: Option<Vec<String>>,
        action: Option<Vec<String>>,
        manage: Option<Vec<String>>,
    ) -> Option<Self> {
        if !is_nsid_or_wildcard(&space_type) {
            return None;
        }

        let authority = authority.unwrap_or_else(|| "self".to_string());
        if authority != "self" && authority != "*" && !is_valid_did(&authority) {
            return None;
        }

        let skey = skey.unwrap_or_else(|| "*".to_string());
        if skey != "*" && !is_valid_record_key(&skey) {
            return None;
        }

        // `*` absorbs the rest; otherwise de-duplicate and sort, matching the
        // reference's normalisation so equal grants compare equal.
        let collection = match collection {
            Some(values) => {
                if values.is_empty() || !values.iter().all(|c| is_nsid_or_wildcard(c)) {
                    return None;
                }
                if values.iter().any(|c| c == "*") {
                    vec!["*".to_string()]
                } else {
                    let mut v = values;
                    v.sort();
                    v.dedup();
                    v
                }
            }
            None => Vec::new(),
        };

        let action = match action {
            Some(values) => {
                if values.is_empty() {
                    return None;
                }
                let parsed = values
                    .iter()
                    .map(|v| SpaceAction::parse(v))
                    .collect::<Option<Vec<_>>>()?;
                SpaceAction::ALL
                    .into_iter()
                    .filter(|a| parsed.contains(a))
                    .collect()
            }
            None => SpaceAction::DEFAULT.to_vec(),
        };

        let manage = match manage {
            Some(values) => {
                if values.is_empty() {
                    return None;
                }
                let parsed = values
                    .iter()
                    .map(|v| ManageOp::parse(v))
                    .collect::<Option<Vec<_>>>()?;
                ManageOp::ALL
                    .into_iter()
                    .filter(|m| parsed.contains(m))
                    .collect()
            }
            None => Vec::new(),
        };

        Some(Self {
            space_type,
            authority,
            skey,
            collection,
            action,
            manage,
        })
    }

    /// Whether this grant authorizes `target` in the named space.
    ///
    /// An unresolved `self` authority matches nothing, because the target's
    /// authority is always a concrete DID. Resolve it first with
    /// [`with_resolved_authority`](Self::with_resolved_authority).
    pub fn matches(
        &self,
        space_type: &str,
        authority: &str,
        skey: &str,
        target: SpaceTarget<'_>,
    ) -> bool {
        if self.space_type != "*" && self.space_type != space_type {
            return false;
        }
        if self.authority != "*" && self.authority != authority {
            return false;
        }
        if self.skey != "*" && self.skey != skey {
            return false;
        }

        match target {
            SpaceTarget::Manage(op) => self.manage.contains(&op),
            // Reads are collection-independent.
            SpaceTarget::Read => self.action.contains(&SpaceAction::Read),
            // `read` implies `read_self`, but not the reverse.
            SpaceTarget::ReadSelf => {
                self.action.contains(&SpaceAction::Read)
                    || self.action.contains(&SpaceAction::ReadSelf)
            }
            SpaceTarget::Write { action, collection } => {
                self.action.contains(&action) && self.collection_allows(collection)
            }
        }
    }

    fn collection_allows(&self, collection: &str) -> bool {
        self.collection.iter().any(|c| c == "*" || c == collection)
    }

    pub fn has_collections(&self) -> bool {
        !self.collection.is_empty()
    }

    pub fn is_self_authority(&self) -> bool {
        self.authority == "self"
    }

    /// Materialize a space type's declared collections into a bare grant.
    ///
    /// Called at token-issuance time: the matcher is context-free and cannot
    /// resolve declarations itself. A `*` space type has no declaration to read,
    /// so callers pass an empty slice and the grant keeps no write targets.
    pub fn with_default_collections(mut self, collections: &[String]) -> Self {
        if self.has_collections() || collections.is_empty() {
            return self;
        }
        self.collection = collections.to_vec();
        self
    }

    /// Resolve an `authority` of `self` to the granting user's DID. Called at
    /// token-issuance time, alongside
    /// [`with_default_collections`](Self::with_default_collections).
    pub fn with_resolved_authority(mut self, user_did: &str) -> Self {
        if self.authority == "self" {
            self.authority = user_did.to_string();
        }
        self
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

#[cfg(test)]
mod space_tests {
    use super::*;

    const DID_A: &str = "did:plc:abcdefghijklmnopqrstuvwx";
    const DID_B: &str = "did:plc:zzzzzzzzzzzzzzzzzzzzzzzz";

    fn write(action: SpaceAction, collection: &str) -> SpaceTarget<'_> {
        SpaceTarget::Write { action, collection }
    }

    #[test]
    fn defaults_match_the_reference_parser() {
        let p = SpacePermission::parse("space:com.example.forum").unwrap();
        assert_eq!(p.space_type, "com.example.forum");
        // Defaults to `self`, so a bare grant covers only the granting user's
        // own spaces of that type.
        assert_eq!(p.authority, "self");
        assert_eq!(p.skey, "*");
        assert_eq!(
            p.action,
            vec![
                SpaceAction::Read,
                SpaceAction::Create,
                SpaceAction::Update,
                SpaceAction::Delete
            ]
        );
        // Empty means no write targets, not "all collections".
        assert!(p.collection.is_empty());
        // An ordinary record grant confers nothing administrative.
        assert!(p.manage.is_empty());
    }

    #[test]
    fn a_wildcard_space_type_is_valid() {
        assert_eq!(
            SpacePermission::parse("space:*?authority=*")
                .unwrap()
                .space_type,
            "*"
        );
    }

    #[test]
    fn space_type_must_be_an_nsid_or_wildcard() {
        assert!(SpacePermission::parse("space:notannsid").is_none());
        assert!(SpacePermission::parse("space:").is_none());
    }

    #[test]
    fn authority_accepts_self_a_did_or_a_wildcard() {
        assert_eq!(
            SpacePermission::parse("space:com.example.forum?authority=*")
                .unwrap()
                .authority,
            "*"
        );
        assert_eq!(
            SpacePermission::parse(&format!("space:com.example.forum?authority={DID_A}"))
                .unwrap()
                .authority,
            DID_A
        );
        // Any DID method; see `is_valid_did`.
        assert!(
            SpacePermission::parse("space:com.example.forum?authority=did:example:xyz").is_some()
        );
        assert!(SpacePermission::parse("space:com.example.forum?authority=notadid").is_none());
    }

    #[test]
    fn skey_takes_record_key_syntax_or_a_wildcard() {
        assert!(SpacePermission::parse("space:com.example.forum?skey=self").is_some());
        assert!(SpacePermission::parse("space:com.example.forum?skey=*").is_some());
        assert!(SpacePermission::parse("space:com.example.forum?skey=.").is_none());
        assert!(SpacePermission::parse("space:com.example.forum?skey=has%20space").is_none());
    }

    #[test]
    fn read_self_is_an_action() {
        // read_self is an OAuth action, not a membership level.
        let p = SpacePermission::parse("space:com.example.forum?action=read_self").unwrap();
        assert_eq!(p.action, vec![SpaceAction::ReadSelf]);
    }

    #[test]
    fn unknown_keys_and_values_are_rejected() {
        assert!(SpacePermission::parse("space:com.example.forum?bogus=1").is_none());
        assert!(SpacePermission::parse("space:com.example.forum?action=bogus").is_none());
        assert!(SpacePermission::parse("space:com.example.forum?manage=read").is_none());
        // Comma-joined values are not the repeated-parameter syntax.
        assert!(SpacePermission::parse("space:com.example.forum?action=read,create").is_none());
        assert!(SpacePermission::parse("space:com.example.forum?action=").is_none());
    }

    #[test]
    fn repeated_values_are_normalised_to_a_canonical_order() {
        // Equal grants must compare equal regardless of how they were written.
        let a =
            SpacePermission::parse("space:com.example.forum?action=delete&action=read").unwrap();
        let b =
            SpacePermission::parse("space:com.example.forum?action=read&action=delete").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.action, vec![SpaceAction::Read, SpaceAction::Delete]);

        let dup = SpacePermission::parse(
            "space:com.example.forum?collection=com.example.b&collection=com.example.a&collection=com.example.a",
        )
        .unwrap();
        assert_eq!(dup.collection, vec!["com.example.a", "com.example.b"]);
    }

    #[test]
    fn a_collection_wildcard_absorbs_the_rest() {
        let p =
            SpacePermission::parse("space:com.example.forum?collection=com.example.a&collection=*")
                .unwrap();
        assert_eq!(p.collection, vec!["*"]);
    }

    #[test]
    fn selection_matches_on_type_authority_and_skey() {
        let p = SpacePermission::parse(&format!(
            "space:com.example.forum?authority={DID_A}&skey=main"
        ))
        .unwrap();

        assert!(p.matches("com.example.forum", DID_A, "main", SpaceTarget::Read));
        assert!(!p.matches("com.example.forum", DID_B, "main", SpaceTarget::Read));
        assert!(!p.matches("com.example.other", DID_A, "main", SpaceTarget::Read));
        assert!(!p.matches("com.example.forum", DID_A, "other", SpaceTarget::Read));
    }

    #[test]
    fn an_unresolved_self_authority_matches_nothing() {
        let p = SpacePermission::parse("space:com.example.forum").unwrap();
        assert!(!p.matches("com.example.forum", DID_A, "main", SpaceTarget::Read));

        let resolved = p.with_resolved_authority(DID_A);
        assert!(resolved.matches("com.example.forum", DID_A, "main", SpaceTarget::Read));
        assert!(!resolved.matches("com.example.forum", DID_B, "main", SpaceTarget::Read));
    }

    #[test]
    fn resolving_authority_leaves_an_explicit_one_alone() {
        let p = SpacePermission::parse("space:com.example.forum?authority=*").unwrap();
        assert_eq!(p.clone().with_resolved_authority(DID_A).authority, "*");
    }

    #[test]
    fn reads_are_collection_independent() {
        // Read access is all-or-nothing at the space boundary: a narrow
        // collection list must not narrow a read.
        let p =
            SpacePermission::parse("space:com.example.forum?authority=*&collection=com.example.a")
                .unwrap();
        assert!(p.matches("com.example.forum", DID_A, "main", SpaceTarget::Read));
    }

    #[test]
    fn read_implies_read_self_but_not_the_reverse() {
        let broad =
            SpacePermission::parse("space:com.example.forum?authority=*&action=read").unwrap();
        assert!(broad.matches("com.example.forum", DID_A, "m", SpaceTarget::ReadSelf));

        let narrow =
            SpacePermission::parse("space:com.example.forum?authority=*&action=read_self").unwrap();
        assert!(!narrow.matches("com.example.forum", DID_A, "m", SpaceTarget::Read));
        assert!(narrow.matches("com.example.forum", DID_A, "m", SpaceTarget::ReadSelf));
    }

    #[test]
    fn writes_are_constrained_by_collection() {
        let p = SpacePermission::parse(
            "space:com.example.forum?authority=*&collection=com.example.a&action=create",
        )
        .unwrap();

        assert!(p.matches(
            "com.example.forum",
            DID_A,
            "m",
            write(SpaceAction::Create, "com.example.a")
        ));
        assert!(!p.matches(
            "com.example.forum",
            DID_A,
            "m",
            write(SpaceAction::Create, "com.example.b")
        ));
        assert!(!p.matches(
            "com.example.forum",
            DID_A,
            "m",
            write(SpaceAction::Update, "com.example.a")
        ));
    }

    #[test]
    fn a_bare_grant_has_no_write_targets_until_collections_are_materialised() {
        let p = SpacePermission::parse("space:com.example.forum?authority=*").unwrap();
        assert!(!p.matches(
            "com.example.forum",
            DID_A,
            "m",
            write(SpaceAction::Create, "com.example.thread")
        ));

        let declared = ["com.example.thread".to_string()];
        let issued = p.with_default_collections(&declared);
        assert!(issued.matches(
            "com.example.forum",
            DID_A,
            "m",
            write(SpaceAction::Create, "com.example.thread")
        ));
        assert!(!issued.matches(
            "com.example.forum",
            DID_A,
            "m",
            write(SpaceAction::Create, "com.example.other")
        ));
    }

    #[test]
    fn materialising_collections_never_widens_an_explicit_list() {
        // An explicit collection list takes precedence over the space type's
        // declaration.
        let p = SpacePermission::parse("space:com.example.forum?collection=com.example.a").unwrap();
        let issued = p.with_default_collections(&["com.example.b".to_string()]);
        assert_eq!(issued.collection, vec!["com.example.a"]);
    }

    #[test]
    fn manage_is_separate_from_record_actions() {
        let p =
            SpacePermission::parse("space:com.example.forum?authority=*&manage=update").unwrap();
        assert!(p.matches(
            "com.example.forum",
            DID_A,
            "m",
            SpaceTarget::Manage(ManageOp::Update)
        ));
        assert!(!p.matches(
            "com.example.forum",
            DID_A,
            "m",
            SpaceTarget::Manage(ManageOp::Delete)
        ));

        // A record grant alone confers nothing administrative.
        let records =
            SpacePermission::parse("space:com.example.forum?authority=*&action=create").unwrap();
        assert!(!records.matches(
            "com.example.forum",
            DID_A,
            "m",
            SpaceTarget::Manage(ManageOp::Create)
        ));
    }
}
