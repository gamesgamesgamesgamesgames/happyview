use crate::AppState;
use crate::auth::middleware::Claims;
use crate::error::AppError;
use crate::spaces::types::{MemberAccess, Space};
use happyview_scopes::SpaceTarget;

/// Check whether the caller may read a specific target repo.
///
/// Space credentials always grant full read (they were already authorized by the
/// credential issuance flow). OAuth/session callers are limited by their membership
/// access level.
pub fn check_read_access(
    caller_did: &str,
    target_repo_did: &str,
    access: MemberAccess,
    has_space_credential: bool,
) -> Result<(), AppError> {
    if has_space_credential {
        return Ok(());
    }
    if !access.can_read() {
        return Err(AppError::Forbidden(
            "you do not have read access to this space".into(),
        ));
    }
    if access.restricted_to_own_records() && caller_did != target_repo_did {
        return Err(AppError::Forbidden(
            "read_self access only permits reading your own repo".into(),
        ));
    }
    Ok(())
}

/// Check whether the caller may call getDelegationToken.
///
/// Requires full `read` access — `read_self` members cannot obtain delegation tokens.
pub fn check_delegation_token_access(
    access: MemberAccess,
    has_space_credential: bool,
) -> Result<(), AppError> {
    if has_space_credential {
        return Ok(());
    }
    if !access.can_read() {
        return Err(AppError::Forbidden(
            "you do not have read access to this space".into(),
        ));
    }
    if access.restricted_to_own_records() {
        return Err(AppError::Forbidden(
            "read_self access does not permit obtaining delegation tokens".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_access_allows_any_repo() {
        assert!(
            check_read_access("did:plc:alice", "did:plc:bob", MemberAccess::READ, false).is_ok()
        );
    }

    #[test]
    fn read_self_member_may_only_read_their_own_repo() {
        assert!(
            check_read_access("did:plc:me", "did:plc:me", MemberAccess::READ_SELF, false).is_ok()
        );
        assert!(
            check_read_access(
                "did:plc:me",
                "did:plc:other",
                MemberAccess::READ_SELF,
                false
            )
            .is_err()
        );
    }

    #[test]
    fn write_without_read_is_representable_and_denies_reads() {
        // A writer need not also be a reader.
        let access = MemberAccess {
            read: false,
            write: true,
            read_self: false,
        };
        assert!(check_read_access("did:plc:me", "did:plc:other", access, false).is_err());
        assert!(access.can_write());
    }

    #[test]
    fn a_space_credential_grants_read_regardless_of_membership() {
        let none = MemberAccess {
            read: false,
            write: false,
            read_self: false,
        };
        assert!(check_read_access("did:plc:me", "did:plc:other", none, true).is_ok());
    }

    #[test]
    fn delegation_tokens_require_unrestricted_read() {
        assert!(check_delegation_token_access(MemberAccess::READ, false).is_ok());
        assert!(check_delegation_token_access(MemberAccess::WRITE, false).is_ok());
        assert!(check_delegation_token_access(MemberAccess::READ_SELF, false).is_err());
    }

    #[test]
    fn union_takes_the_most_permissive_on_each_axis() {
        let merged = MemberAccess::READ_SELF.union(MemberAccess::READ);
        assert!(merged.can_read());
        assert!(!merged.restricted_to_own_records());

        let merged = MemberAccess::READ_SELF.union(MemberAccess::READ_SELF);
        assert!(merged.restricted_to_own_records());

        let merged = MemberAccess::READ.union(MemberAccess::WRITE);
        assert!(merged.can_write());
    }
}

// ---------------------------------------------------------------------------
// OAuth `space:` scope enforcement
// ---------------------------------------------------------------------------

/// Refuse the request unless the caller's session holds a covering `space:` grant.
///
/// Two independent gates guard a space, and both must pass: the member list says
/// what the *user* may do, and the `space:` scope says what the *app* may do on
/// their behalf. A member with read access still cannot read through an app that
/// was never granted it.
///
/// Returns `Ok` when the granted scopes are not knowable: a first-party cookie
/// session, where the user acts directly rather than through an app, so there is
/// no delegated grant to check. The proxy path in `src/xrpc/mod.rs` skips in the
/// same case, but there the user's PDS still enforces scopes; here HappyView is
/// the authority. The skip therefore covers only sessions that carry no scope at
/// all, never a session whose scope lacks the grant.
pub async fn require_space_scope(
    state: &AppState,
    claims: &Claims,
    space: &Space,
    target: SpaceTarget<'_>,
) -> Result<(), AppError> {
    require_space_scope_at(
        state,
        claims,
        &space.type_nsid,
        &space.authority_did,
        &space.skey,
        target,
    )
    .await
}

/// [`require_space_scope`] for a space named by its address, which need not
/// exist yet: `createSpace` checks `manage=create` before there is a space.
/// A space created here gets a DID of its own, unknown until it exists, so a
/// grant covers creating one only with `authority=*`; callers pass `*`.
///
/// A grant that names no collections gets the ones the space type's
/// declaration lists, as Proposal 0016 defines the default, resolved now
/// rather than when the session was granted.
pub async fn require_space_scope_at(
    state: &AppState,
    claims: &Claims,
    space_type: &str,
    authority: &str,
    skey: &str,
    target: SpaceTarget<'_>,
) -> Result<(), AppError> {
    let (Some(client_key), Some(dpop_key_id)) = (claims.client_key(), claims.dpop_key_id()) else {
        return Ok(());
    };

    let api_client_id = crate::repo::session::get_dpop_client_id(state, client_key).await?;
    let Some(scopes) = crate::oauth::sessions::get_dpop_session_scopes(
        &state.db,
        state.db_backend,
        &api_client_id,
        dpop_key_id,
    )
    .await?
    else {
        return Ok(());
    };

    let declared = state
        .lexicons
        .get_space_declaration(space_type)
        .await
        .and_then(|d| d.space_collections)
        .unwrap_or_default();
    let covered_by = |scopes: &str| {
        grants_cover(
            scopes,
            claims.did(),
            &declared,
            space_type,
            authority,
            skey,
            target,
        )
    };
    if covered_by(&scopes) {
        // The registered scopes are the app's word; the session also needs
        // what its authorization server granted to cover the target. A grant
        // that cannot be read refuses rather than falling back.
        let encryption_key = state
            .config
            .keyring()
            .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;
        let granted = crate::oauth::pds_write::verified_granted_scope(
            &state.http,
            &state.db,
            state.db_backend,
            &encryption_key,
            &state.oauth,
            &state.config.plc_url,
            &api_client_id,
            claims.did(),
            dpop_key_id,
        )
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "could not confirm a session's grant for a space request");
            AppError::Forbidden(
                "HappyView could not confirm this session's grant with its authorization \
                 server, so it is not authorized for this space. Sign in again."
                    .into(),
            )
        })?;
        if covered_by(&granted) {
            return Ok(());
        }
    }

    let needed = match target {
        SpaceTarget::Manage(op) => format!("&manage={}", op.as_str()),
        _ => String::new(),
    };
    Err(AppError::Forbidden(format!(
        "this session is not authorized for this space: it needs a space: grant covering \
         {space_type}?authority={authority}&skey={skey}{needed}. Scopes are fixed when a \
         session is created, so this needs a new authorization."
    )))
}

/// Whether the session's `scopes` cover `target` in the space at
/// (`space_type`, `authority`, `skey`), for the user `user_did`, with a grant
/// that names no collections taking `declared` for a grant of this space type.
pub(crate) fn grants_cover(
    scopes: &str,
    user_did: &str,
    declared: &[String],
    space_type: &str,
    authority: &str,
    skey: &str,
    target: SpaceTarget<'_>,
) -> bool {
    // A bare grant takes the declaration's collections as its own. Only NSIDs
    // count: the declaration is the lexicon author's, not the user's consent,
    // so a `*` there must not widen a grant to every collection.
    let declared: Vec<String> = declared
        .iter()
        .filter(|c| happyview_nsid::validate_nsid(c).is_ok())
        .cloned()
        .collect();
    scopes
        .split_whitespace()
        .filter_map(happyview_scopes::SpacePermission::parse)
        .map(|p| {
            // A `*` space type has no declaration to draw a default from.
            let defaults: &[String] = if p.space_type == space_type {
                &declared
            } else {
                &[]
            };
            p.with_resolved_authority(user_did)
                .with_default_collections(defaults)
        })
        .any(|p| p.matches(space_type, authority, skey, target))
}

#[cfg(test)]
mod grant_tests {
    use super::grants_cover;
    use happyview_scopes::{ManageOp, SpaceAction, SpaceTarget};

    const USER: &str = "did:plc:user";
    const SPACE: &str = "did:plc:space";

    fn covers(scopes: &str, declared: &[&str], target: SpaceTarget<'_>) -> bool {
        let declared: Vec<String> = declared.iter().map(|s| s.to_string()).collect();
        grants_cover(
            scopes,
            USER,
            &declared,
            "com.example.forum",
            SPACE,
            "main",
            target,
        )
    }

    fn write(collection: &str) -> SpaceTarget<'_> {
        SpaceTarget::Write {
            action: SpaceAction::Create,
            collection,
        }
    }

    #[test]
    fn a_bare_grant_writes_the_declared_collections() {
        let grant = "space:com.example.forum?authority=*";
        assert!(covers(
            grant,
            &["com.example.post"],
            write("com.example.post")
        ));
        assert!(!covers(
            grant,
            &["com.example.post"],
            write("com.example.other")
        ));
        assert!(!covers(grant, &[], write("com.example.post")));
    }

    #[test]
    fn a_declared_wildcard_or_non_nsid_is_no_default() {
        let grant = "space:com.example.forum?authority=*";
        for declared in ["*", "com.example.*", "not an nsid"] {
            assert!(
                !covers(grant, &[declared], write("com.example.post")),
                "{declared}"
            );
        }
        assert!(covers(
            grant,
            &["*", "com.example.post"],
            write("com.example.post")
        ));
    }

    #[test]
    fn named_collections_replace_the_default() {
        let grant = "space:com.example.forum?authority=*&collection=com.example.other";
        assert!(covers(
            grant,
            &["com.example.post"],
            write("com.example.other")
        ));
        assert!(!covers(
            grant,
            &["com.example.post"],
            write("com.example.post")
        ));
    }

    #[test]
    fn manage_needs_its_own_parameter() {
        let target = SpaceTarget::Manage(ManageOp::Update);
        assert!(!covers("space:com.example.forum?authority=*", &[], target));
        assert!(covers(
            "space:com.example.forum?authority=*&manage=update",
            &[],
            target
        ));
        assert!(!covers(
            "space:com.example.forum?authority=*&manage=delete",
            &[],
            target
        ));
    }

    #[test]
    fn a_bare_grant_covers_only_the_users_own_did() {
        assert!(!covers(
            "space:com.example.forum?manage=update",
            &[],
            SpaceTarget::Manage(ManageOp::Update)
        ));
    }
}

#[cfg(test)]
pub(crate) mod space_scope_gate_tests {
    use super::*;
    use crate::db::adapt_sql;
    use crate::spaces::types::{AppAccess, Policy, SpaceConfig};

    pub(crate) const USER: &str = "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa";

    fn a_space() -> Space {
        Space {
            id: "sp1".into(),
            did: USER.into(),
            authority_did: USER.into(),
            creator_did: USER.into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            display_name: None,
            description: None,
            read_policy: Policy::MemberList,
            write_policy: Policy::MemberList,
            app_access: AppAccess::Open,
            config: SpaceConfig::default(),
            revision: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    /// Seed an API client plus a DPoP session carrying `scopes`, which its
    /// authorization server granted in full, and return claims naming them.
    pub(crate) async fn session_with_scopes(state: &AppState, scopes: &str) -> Claims {
        session_with_grant(state, scopes, Some(scopes)).await
    }

    /// [`session_with_scopes`] for a session registered with `scopes` whose
    /// authorization server granted `granted`, cached with a live access token.
    /// `None` leaves no grant on record and the token expired, so reading the
    /// grant needs a refresh this seeded session cannot make.
    pub(crate) async fn session_with_grant(
        state: &AppState,
        scopes: &str,
        granted: Option<&str>,
    ) -> Claims {
        let now = crate::db::now_rfc3339();
        let client_id = uuid::Uuid::new_v4().to_string();
        let client_key = format!("hvc_{}", uuid::Uuid::new_v4().simple());
        let dpop_key_id = uuid::Uuid::new_v4().to_string();

        let sql = adapt_sql(
            "INSERT INTO happyview_api_clients (id, client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, scopes, client_type, is_active, created_by, created_at, updated_at) VALUES (?, ?, '', 'test', ?, '', '[]', '', 'confidential', 1, ?, ?, ?)",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(&client_id)
            .bind(&client_key)
            .bind(format!("https://client.test/{client_id}"))
            .bind(USER)
            .bind(&now)
            .bind(&now)
            .execute(&state.db)
            .await
            .expect("seed api client");

        let sql = adapt_sql(
            "INSERT INTO happyview_dpop_keys (id, provision_id, api_client_id, private_key_enc, jwk_thumbprint, created_at) VALUES (?, ?, ?, X'00', '', ?)",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(&dpop_key_id)
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&client_id)
            .bind(&now)
            .execute(&state.db)
            .await
            .expect("seed dpop key");

        let sql = adapt_sql(
            "INSERT INTO happyview_dpop_sessions (id, api_client_id, dpop_key_id, user_did, access_token_enc, scopes, granted_scope, token_expires_at, created_at, updated_at) VALUES (?, ?, ?, ?, X'00', ?, ?, ?, ?, ?)",
            state.db_backend,
        );
        let expires_at = match granted {
            Some(_) => (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            None => (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339(),
        };
        crate::db::query(&sql)
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&client_id)
            .bind(&dpop_key_id)
            .bind(USER)
            .bind(scopes)
            .bind(granted)
            .bind(&expires_at)
            .bind(&now)
            .bind(&now)
            .execute(&state.db)
            .await
            .expect("seed dpop session");

        Claims::for_test(USER, Some(&client_key), Some(&dpop_key_id))
    }

    async fn test_state() -> AppState {
        let pool = crate::test_support::migrated_memory_pool().await;
        crate::test_support::test_state_with_pool(pool)
    }

    #[tokio::test]
    async fn a_covering_grant_is_allowed() {
        let state = test_state().await;
        let claims = session_with_scopes(
            &state,
            &format!("atproto space:com.example.forum?authority={USER}"),
        )
        .await;

        require_space_scope(&state, &claims, &a_space(), SpaceTarget::Read)
            .await
            .expect("a covering grant must pass");
    }

    #[tokio::test]
    async fn a_registered_scope_the_server_did_not_grant_is_refused() {
        // The app registered a space grant; its authorization server granted
        // only `atproto`. The registered scope alone authorizes nothing.
        let state = test_state().await;
        let claims = session_with_grant(
            &state,
            &format!("atproto space:com.example.forum?authority={USER}"),
            Some("atproto"),
        )
        .await;

        let err = require_space_scope(&state, &claims, &a_space(), SpaceTarget::Read)
            .await
            .expect_err("a grant the server never made must be refused");
        assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_grant_that_cannot_be_read_is_refused() {
        let state = test_state().await;
        let claims = session_with_grant(
            &state,
            &format!("atproto space:com.example.forum?authority={USER}"),
            None,
        )
        .await;

        let err = require_space_scope(&state, &claims, &a_space(), SpaceTarget::Read)
            .await
            .expect_err("an unconfirmed grant must not fall back to the registered scope");
        assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    }

    #[tokio::test]
    async fn the_server_grant_alone_does_not_widen_the_registered_scope() {
        // Both must cover the target: a broad grant does not extend a session
        // the app registered more narrowly.
        let state = test_state().await;
        let claims = session_with_grant(
            &state,
            "atproto",
            Some(&format!("atproto space:com.example.forum?authority={USER}")),
        )
        .await;

        assert!(
            require_space_scope(&state, &claims, &a_space(), SpaceTarget::Read)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_self_grant_covers_the_callers_own_space() {
        // `authority` defaults to `self`, which is how an app asks for its user's
        // own spaces and how the PDS stores the grant.
        let state = test_state().await;
        let claims = session_with_scopes(&state, "atproto space:com.example.forum").await;

        require_space_scope(&state, &claims, &a_space(), SpaceTarget::Read)
            .await
            .expect("a self grant must cover a space whose authority is the caller");
    }

    #[tokio::test]
    async fn membership_is_not_enough_without_a_grant() {
        // The two gates are independent: this session belongs to the space
        // authority itself, and still cannot read through an app that was never
        // granted space access.
        let state = test_state().await;
        let claims = session_with_scopes(&state, "atproto repo:com.example.post").await;

        let err = require_space_scope(&state, &claims, &a_space(), SpaceTarget::Read)
            .await
            .expect_err("a session with no space: grant must be refused");
        assert!(matches!(err, AppError::Forbidden(_)));
    }

    #[tokio::test]
    async fn a_grant_for_another_space_type_does_not_carry_over() {
        let state = test_state().await;
        let claims =
            session_with_scopes(&state, &format!("space:com.example.other?authority={USER}")).await;

        assert!(
            require_space_scope(&state, &claims, &a_space(), SpaceTarget::Read)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn read_self_does_not_confer_whole_space_read() {
        // The narrower grant must not reach the rest of the space, which is what
        // gates getDelegationToken.
        let state = test_state().await;
        let claims = session_with_scopes(
            &state,
            &format!("space:com.example.forum?authority={USER}&action=read_self"),
        )
        .await;

        let space = a_space();
        assert!(
            require_space_scope(&state, &claims, &space, SpaceTarget::Read)
                .await
                .is_err()
        );
        require_space_scope(&state, &claims, &space, SpaceTarget::ReadSelf)
            .await
            .expect("read_self covers an own-repo read");
    }

    #[tokio::test]
    async fn a_session_with_no_knowable_scopes_is_not_gated() {
        let state = test_state().await;
        let claims = Claims::for_test(USER, None, None);

        require_space_scope(&state, &claims, &a_space(), SpaceTarget::Read)
            .await
            .expect("a cookie session has no scope to check");
    }
}
