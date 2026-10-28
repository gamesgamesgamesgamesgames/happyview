//! The OAuth grant that lets an app use the admin API.
//!
//! An app's DPoP session belongs to the app, and an admin signing in to one
//! must not hand it the instance by default. So an app manages an instance only
//! when the user approved `include:dev.happyview.adminFull` at sign-in. That
//! permission set holds one permission, `rpc` on [`ADMIN_LXM`] with any
//! audience; the authorization server expands the include into it, and that
//! expanded permission is what [`grants_admin`] looks for. `/admin` is not
//! XRPC, so the method is a name for the grant rather than a route.
//!
//! The same method gates a service-auth JWT, which a PDS mints for an OAuth app
//! only when its session holds a matching `rpc` permission.

/// The method the permission set grants. Published, with the set, under
/// happyview.dev's lexicon authority.
pub const ADMIN_LXM: &str = "dev.happyview.admin.manage";

/// The permission set an app includes to ask for it.
pub const PERMISSION_SET: &str = "dev.happyview.adminFull";

/// The permission-set lexicon for [`PERMISSION_SET`], exactly as published.
pub fn builtin_permission_set(nsid: &str) -> Option<serde_json::Value> {
    (nsid == PERMISSION_SET).then(|| {
        serde_json::from_str(include_str!("admin_permission_set.json"))
            .expect("the built-in permission set is valid JSON")
    })
}

/// Whether a scope the authorization server granted lets its holder use the
/// admin API.
///
/// Only an explicit `rpc` permission naming [`ADMIN_LXM`] counts. Neither a
/// wildcard method nor `transition:generic` does, though both would pass
/// `ScopePermissions::allows_rpc`: most apps hold `transition:generic`, and
/// none of them asked for the instance.
pub fn grants_admin(granted_scope: &str) -> bool {
    happyview_scopes::parse_scope_list(granted_scope)
        .iter()
        .filter_map(|s| happyview_scopes::RpcPermission::parse(s))
        .any(|p| p.aud == "*" && p.lxm.iter().any(|l| l == ADMIN_LXM))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_expanded_permission_set_grants_admin() {
        assert!(grants_admin("atproto rpc:dev.happyview.admin.manage?aud=*"));
        assert!(grants_admin(
            "atproto rpc?lxm=dev.happyview.admin.manage&lxm=com.example.other&aud=*"
        ));
    }

    /// The document published under happyview.dev, expanded as a server
    /// expands it, is what `grants_admin` accepts.
    #[test]
    fn the_published_permission_set_expands_to_the_grant() {
        let doc = builtin_permission_set(PERMISSION_SET).unwrap();
        assert_eq!(doc["id"], PERMISSION_SET);

        let set = happyview_scopes::LexPermissionSet {
            permissions: doc["defs"]["main"]["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(crate::oauth::client_auth::lex_permission)
                .collect(),
        };
        let include =
            happyview_scopes::IncludeScope::parse(&format!("include:{PERMISSION_SET}")).unwrap();
        let expanded: Vec<String> = include
            .expand(&set)
            .into_iter()
            .map(|p| match p {
                happyview_scopes::IncludedPermission::Rpc(p) => {
                    format!("rpc:{}?aud={}", p.lxm.join(","), p.aud)
                }
                other => panic!("unexpected permission {other:?}"),
            })
            .collect();
        assert_eq!(expanded, [format!("rpc:{ADMIN_LXM}?aud=*")]);
        assert!(grants_admin(&format!("atproto {}", expanded[0])));
    }

    #[test]
    fn nothing_broader_or_narrower_grants_admin() {
        for scope in [
            "atproto",
            "atproto transition:generic",
            "atproto rpc:*?aud=did:web:example.com%23happyview",
            "atproto rpc:dev.happyview.admin.manage?aud=did:web:example.com%23happyview",
            "atproto rpc:dev.happyview.admin.other?aud=*",
            // The include itself: a server returns it expanded, so an
            // unexpanded one is not something a server granted.
            "atproto include:dev.happyview.adminFull",
        ] {
            assert!(!grants_admin(scope), "{scope}");
        }
    }
}
