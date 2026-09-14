use crate::AppState;
use crate::auth::middleware::Claims;
use crate::error::AppError;
use crate::spaces::types::{MemberAccess, Space};
use happyview_scopes::{ScopePermissions, SpaceTarget};

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

    let granted = ScopePermissions::parse(&scopes);
    if granted.allows_space(&space.type_nsid, &space.authority_did, &space.skey, target) {
        return Ok(());
    }

    Err(AppError::Forbidden(format!(
        "this session is not authorized for this space: it needs a space: grant covering \
         {}?authority={}&skey={}. Scopes are fixed when a session is created, so this needs \
         a new authorization.",
        space.type_nsid, space.authority_did, space.skey
    )))
}

#[cfg(test)]
mod space_scope_gate_tests {
    use super::*;
    use crate::db::adapt_sql;
    use crate::spaces::types::{AppAccess, Policy, SpaceConfig};

    const USER: &str = "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa";

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

    /// Seed an API client plus a DPoP session carrying `scopes`, and return
    /// claims naming them.
    async fn session_with_scopes(state: &AppState, scopes: &str) -> Claims {
        let now = crate::db::now_rfc3339();
        let client_id = uuid::Uuid::new_v4().to_string();
        let client_key = format!("hvc_{}", uuid::Uuid::new_v4().simple());
        let dpop_key_id = uuid::Uuid::new_v4().to_string();

        let sql = adapt_sql(
            "INSERT INTO happyview_api_clients (id, client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, scopes, client_type, is_active, created_by, created_at, updated_at) VALUES (?, ?, '', 'test', '', '', '[]', '', 'confidential', 1, ?, ?, ?)",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(&client_id)
            .bind(&client_key)
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
            "INSERT INTO happyview_dpop_sessions (id, api_client_id, dpop_key_id, user_did, access_token_enc, scopes, created_at, updated_at) VALUES (?, ?, ?, ?, X'00', ?, ?, ?)",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&client_id)
            .bind(&dpop_key_id)
            .bind(USER)
            .bind(scopes)
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
