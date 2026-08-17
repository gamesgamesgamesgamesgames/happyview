//! `include:<nsid>[?aud=<did>]` — expanding a lexicon-defined permission set.
//!
//! Everything here is pure: `expand` takes an **already-resolved** permission
//! set document. Fetching the lexicon for an NSID is the caller's job, because
//! only the caller has a lexicon registry. That split is deliberate — the parts
//! that need I/O are trivial, and the parts that are security-critical are not.

use crate::resources::{RepoPermission, RpcPermission, is_absolute_did_ref};
use crate::syntax::ScopeSyntax;

/// One value in a lexicon permission: the arity matters, because a scalar
/// supplied where a list is expected (or vice versa) invalidates the
/// permission rather than being coerced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexValue {
    Scalar(String),
    List(Vec<String>),
    Bool(bool),
}

/// A single `permission` entry inside a permission-set lexicon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexPermission {
    pub resource: String,
    /// Every key other than `type` and `resource`, which are reserved.
    pub params: Vec<(String, LexValue)>,
}

impl LexPermission {
    fn get(&self, key: &str) -> Option<&LexValue> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    fn keys(&self) -> Vec<&str> {
        self.params.iter().map(|(k, _)| k.as_str()).collect()
    }

    fn list(&self, key: &str) -> Option<Vec<String>> {
        match self.get(key) {
            Some(LexValue::List(v)) => Some(v.clone()),
            _ => None,
        }
    }

    fn scalar(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Some(LexValue::Scalar(v)) => Some(v.clone()),
            _ => None,
        }
    }

    fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(LexValue::Bool(v)) => Some(*v),
            _ => None,
        }
    }
}

/// A `permission-set` lexicon's `permissions` array.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LexPermissionSet {
    pub permissions: Vec<LexPermission>,
}

/// A permission yielded by expanding an `include:` scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncludedPermission {
    Repo(RepoPermission),
    Rpc(RpcPermission),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeScope {
    pub nsid: String,
    /// The audience `inheritAud` permissions adopt, if the scope supplied one.
    pub aud: Option<String>,
}

impl IncludeScope {
    pub fn parse(scope: &str) -> Option<Self> {
        let syntax = ScopeSyntax::parse(scope);
        if syntax.prefix != "include" {
            return None;
        }
        if !syntax.keys().iter().all(|k| ["nsid", "aud"].contains(k)) {
            return None;
        }
        if syntax.positional.is_some() && syntax.get_multi("nsid").is_some() {
            return None;
        }

        let nsid = match syntax.get_single("nsid").ok()? {
            Some(v) => v.to_string(),
            None => syntax.positional.clone()?,
        };
        if happyview_nsid::validate_nsid(&nsid).is_err() {
            return None;
        }

        let aud = syntax.get_single("aud").ok()?.map(str::to_string);
        if let Some(ref a) = aud
            && !is_absolute_did_ref(a)
        {
            return None;
        }

        Some(Self { nsid, aud })
    }

    /// Expand against a resolved permission-set document.
    ///
    /// Permissions that fail validation or containment are **dropped**, not
    /// errors — matching the reference, which skips them and continues.
    pub fn expand(&self, set: &LexPermissionSet) -> Vec<IncludedPermission> {
        set.permissions
            .iter()
            .filter_map(|p| self.expand_one(p))
            .filter(|p| self.is_allowed(p))
            .collect()
    }

    fn expand_one(&self, permission: &LexPermission) -> Option<IncludedPermission> {
        match permission.resource.as_str() {
            "repo" => {
                if !permission
                    .keys()
                    .iter()
                    .all(|k| ["collection", "action"].contains(k))
                {
                    return None;
                }
                let collection = permission.list("collection")?;
                let action = permission.list("action");
                RepoPermission::from_parts(collection, action).map(IncludedPermission::Repo)
            }
            "rpc" => {
                let declared_aud = permission.scalar("aud");

                // A permission set may not pin a concrete audience; only `*` or
                // nothing. Otherwise a set could direct a caller's credentials
                // at a service of its own choosing.
                if let Some(ref a) = declared_aud
                    && a != "*"
                {
                    return None;
                }

                let inherit = permission.bool("inheritAud").unwrap_or(false);
                let (aud, allowed_keys): (Option<String>, &[&str]) =
                    if inherit && declared_aud.is_none() && self.aud.is_some() {
                        // `inheritAud` is consumed here, so it is not an unknown key.
                        (self.aud.clone(), &["lxm", "aud", "inheritAud"])
                    } else {
                        // Otherwise `inheritAud` stays on the permission and is
                        // an unrecognised parameter, which invalidates it. That
                        // is why `inheritAud` with no `?aud=` on the include
                        // yields nothing at all.
                        (declared_aud, &["lxm", "aud"])
                    };

                if !permission.keys().iter().all(|k| allowed_keys.contains(k)) {
                    return None;
                }

                let lxm = permission.list("lxm")?;
                RpcPermission::from_parts(lxm, aud).map(IncludedPermission::Rpc)
            }
            _ => None,
        }
    }

    /// A permission set may only grant NSIDs under its own authority group.
    ///
    /// Without this, publishing a permission-set lexicon would be enough to
    /// vouch for another authority's collections — `com.attacker.authBasic`
    /// could hand out `app.bsky.feed.post`. The reference calls this out as a
    /// security feature and is deliberately strict about it.
    fn is_allowed(&self, permission: &IncludedPermission) -> bool {
        match permission {
            IncludedPermission::Repo(p) => {
                p.collection.iter().all(|c| self.is_parent_authority_of(c))
            }
            IncludedPermission::Rpc(p) => p.lxm.iter().all(|l| self.is_parent_authority_of(l)),
        }
    }

    /// True when `other` sits under this scope's NSID authority group — the
    /// lexicon NSID up to and including its final dot. A wildcard is never
    /// under any authority.
    pub fn is_parent_authority_of(&self, other: &str) -> bool {
        if other == "*" {
            return false;
        }
        let Some(group_prefix_end) = self.nsid.rfind('.') else {
            // `parse` requires a valid NSID, which always has a dot.
            return false;
        };
        if other.len() <= group_prefix_end + 1 {
            return false;
        }
        self.nsid.as_bytes()[..=group_prefix_end] == other.as_bytes()[..=group_prefix_end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_perm(collections: &[&str]) -> LexPermission {
        LexPermission {
            resource: "repo".into(),
            params: vec![(
                "collection".into(),
                LexValue::List(collections.iter().map(|s| s.to_string()).collect()),
            )],
        }
    }

    fn rpc_perm(lxms: &[&str], extra: Vec<(String, LexValue)>) -> LexPermission {
        let mut params = vec![(
            "lxm".into(),
            LexValue::List(lxms.iter().map(|s| s.to_string()).collect()),
        )];
        params.extend(extra);
        LexPermission {
            resource: "rpc".into(),
            params,
        }
    }

    #[test]
    fn parses_bare_and_aud_forms() {
        let s = IncludeScope::parse("include:com.example.authBasic").unwrap();
        assert_eq!(s.nsid, "com.example.authBasic");
        assert_eq!(s.aud, None);

        let s =
            IncludeScope::parse("include:com.example.authBasic?aud=did:web:x.com%23svc").unwrap();
        assert_eq!(s.aud.as_deref(), Some("did:web:x.com#svc"));
    }

    #[test]
    fn rejects_bad_nsid_and_bad_aud() {
        assert!(IncludeScope::parse("include:").is_none());
        assert!(IncludeScope::parse("include:notannsid").is_none());
        assert!(IncludeScope::parse("include:com.example.a?aud=notadid").is_none());
    }

    /// The containment check, stated as the scenario it prevents.
    #[test]
    fn drops_permissions_outside_the_sets_authority() {
        let inc = IncludeScope::parse("include:com.example.authBasic").unwrap();
        let set = LexPermissionSet {
            permissions: vec![
                repo_perm(&["com.example.profile"]),
                repo_perm(&["app.bsky.feed.post"]),
            ],
        };
        let out = inc.expand(&set);
        assert_eq!(out.len(), 1);
        match &out[0] {
            IncludedPermission::Repo(p) => assert_eq!(p.collection, vec!["com.example.profile"]),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_wildcard_is_never_under_an_authority() {
        let inc = IncludeScope::parse("include:com.example.authBasic").unwrap();
        assert!(!inc.is_parent_authority_of("*"));
        let set = LexPermissionSet {
            permissions: vec![repo_perm(&["*"])],
        };
        assert!(inc.expand(&set).is_empty());
    }

    #[test]
    fn sibling_authority_is_not_a_parent() {
        let inc = IncludeScope::parse("include:com.example.authBasic").unwrap();
        assert!(inc.is_parent_authority_of("com.example.anything"));
        assert!(!inc.is_parent_authority_of("com.examples.anything"));
        assert!(!inc.is_parent_authority_of("com.exampl.anything"));
    }

    #[test]
    fn rpc_without_aud_expands_to_nothing() {
        let inc = IncludeScope::parse("include:com.example.authBasic").unwrap();
        let set = LexPermissionSet {
            permissions: vec![rpc_perm(&["com.example.getFeed"], vec![])],
        };
        assert!(inc.expand(&set).is_empty());
    }

    #[test]
    fn rpc_with_wildcard_aud_expands() {
        let inc = IncludeScope::parse("include:com.example.authBasic").unwrap();
        let set = LexPermissionSet {
            permissions: vec![rpc_perm(
                &["com.example.getFeed"],
                vec![("aud".into(), LexValue::Scalar("*".into()))],
            )],
        };
        let out = inc.expand(&set);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn inherit_aud_takes_the_includes_audience() {
        let inc =
            IncludeScope::parse("include:com.example.authBasic?aud=did:web:x.com%23svc").unwrap();
        let set = LexPermissionSet {
            permissions: vec![rpc_perm(
                &["com.example.getFeed"],
                vec![("inheritAud".into(), LexValue::Bool(true))],
            )],
        };
        let out = inc.expand(&set);
        match &out[0] {
            IncludedPermission::Rpc(p) => assert_eq!(p.aud, "did:web:x.com#svc"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn inherit_aud_without_an_include_audience_expands_to_nothing() {
        let inc = IncludeScope::parse("include:com.example.authBasic").unwrap();
        let set = LexPermissionSet {
            permissions: vec![rpc_perm(
                &["com.example.getFeed"],
                vec![("inheritAud".into(), LexValue::Bool(true))],
            )],
        };
        assert!(inc.expand(&set).is_empty());
    }

    #[test]
    fn a_concrete_aud_in_a_permission_set_is_refused() {
        let inc = IncludeScope::parse("include:com.example.authBasic").unwrap();
        let set = LexPermissionSet {
            permissions: vec![rpc_perm(
                &["com.example.getFeed"],
                vec![(
                    "aud".into(),
                    LexValue::Scalar("did:web:evil.example.com#svc".into()),
                )],
            )],
        };
        assert!(inc.expand(&set).is_empty());
    }

    #[test]
    fn non_repo_rpc_resources_are_dropped() {
        let inc = IncludeScope::parse("include:com.example.authBasic").unwrap();
        let set = LexPermissionSet {
            permissions: vec![LexPermission {
                resource: "account".into(),
                params: vec![("attr".into(), LexValue::Scalar("email".into()))],
            }],
        };
        assert!(inc.expand(&set).is_empty());
    }

    #[test]
    fn a_scalar_where_a_list_is_required_is_dropped() {
        let inc = IncludeScope::parse("include:com.example.authBasic").unwrap();
        let set = LexPermissionSet {
            permissions: vec![LexPermission {
                resource: "repo".into(),
                params: vec![(
                    "collection".into(),
                    LexValue::Scalar("com.example.profile".into()),
                )],
            }],
        };
        assert!(inc.expand(&set).is_empty());
    }
}
