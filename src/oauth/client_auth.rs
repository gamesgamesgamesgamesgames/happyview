use sha2::{Digest, Sha256};

use crate::db::{DatabaseBackend, adapt_sql};
use crate::error::AppError;

/// Resolved API client identity for DPoP operations.
pub struct ResolvedClient {
    pub id: String,
    pub client_key: String,
    pub client_type: String,
    pub scopes: String,
    pub allowed_origins: Option<Vec<String>>,
}

/// Authenticate a confidential client using client_key + client_secret.
pub async fn authenticate_confidential(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    client_key: &str,
    client_secret: &str,
) -> Result<ResolvedClient, AppError> {
    let secret_hash = hex::encode(Sha256::digest(client_secret.as_bytes()));

    let sql = adapt_sql(
        "SELECT id, client_key, client_type, scopes, allowed_origins, client_secret_hash FROM happyview_api_clients WHERE client_key = ? AND is_active = 1",
        backend,
    );

    let row: Option<(String, String, String, String, Option<String>, String)> =
        crate::db::query_as(&sql)
            .bind(client_key)
            .fetch_optional(pool)
            .await
            .map_err(|e| AppError::Internal(format!("client lookup failed: {e}")))?;

    let (id, key, client_type, scopes, origins_json, stored_hash) =
        row.ok_or_else(|| AppError::Auth("invalid client credentials".into()))?;

    if !crate::constant_time::ct_eq_str(&stored_hash, &secret_hash) {
        return Err(AppError::Auth("invalid client credentials".into()));
    }

    if client_type != "confidential" {
        return Err(AppError::Auth(
            "this endpoint requires confidential client authentication".into(),
        ));
    }

    let allowed_origins =
        origins_json.map(|json| serde_json::from_str::<Vec<String>>(&json).unwrap_or_default());

    Ok(ResolvedClient {
        id,
        client_key: key,
        client_type,
        scopes,
        allowed_origins,
    })
}

/// Authenticate a public client using client_key + origin validation.
/// Returns the client but does NOT verify PKCE — that's done at session registration.
pub async fn authenticate_public(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    client_key: &str,
    origin: Option<&str>,
) -> Result<ResolvedClient, AppError> {
    let sql = adapt_sql(
        "SELECT id, client_key, client_type, scopes, allowed_origins FROM happyview_api_clients WHERE client_key = ? AND is_active = 1",
        backend,
    );

    let row: Option<(String, String, String, String, Option<String>)> = crate::db::query_as(&sql)
        .bind(client_key)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("client lookup failed: {e}")))?;

    let (id, key, client_type, scopes, origins_json) =
        row.ok_or_else(|| AppError::Auth("unknown client".into()))?;

    if client_type != "public" {
        return Err(AppError::Auth(
            "this client is not registered as a public client".into(),
        ));
    }

    // Validate origin if the client has allowed_origins configured
    if let Some(ref origins_str) = origins_json {
        let allowed: Vec<String> = serde_json::from_str(origins_str).unwrap_or_default();
        if !allowed.is_empty() {
            match origin {
                Some(o) if allowed.iter().any(|a| a == o) => {}
                Some(o) => {
                    tracing::warn!(client_key, origin = o, "Origin mismatch for public client");
                    return Err(AppError::Auth("origin not allowed for this client".into()));
                }
                None => {
                    tracing::warn!(client_key, "No Origin header for public client");
                    return Err(AppError::Auth(
                        "Origin header required for public clients".into(),
                    ));
                }
            }
        }
    }

    let allowed_origins =
        origins_json.map(|json| serde_json::from_str::<Vec<String>>(&json).unwrap_or_default());

    Ok(ResolvedClient {
        id,
        client_key: key,
        client_type,
        scopes,
        allowed_origins,
    })
}

/// Resolve an API client by client_key only (no secret verification).
/// Used when the caller has already been authenticated by other means (e.g. DPoP proof).
pub async fn resolve_client_by_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    client_key: &str,
) -> Result<ResolvedClient, AppError> {
    let sql = adapt_sql(
        "SELECT id, client_key, client_type, scopes, allowed_origins FROM happyview_api_clients WHERE client_key = ? AND is_active = 1",
        backend,
    );

    let row: Option<(String, String, String, String, Option<String>)> = crate::db::query_as(&sql)
        .bind(client_key)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("client lookup failed: {e}")))?;

    let (id, key, client_type, scopes, origins_json) =
        row.ok_or_else(|| AppError::Auth("unknown client".into()))?;

    let allowed_origins =
        origins_json.map(|json| serde_json::from_str::<Vec<String>>(&json).unwrap_or_default());

    Ok(ResolvedClient {
        id,
        client_key: key,
        client_type,
        scopes,
        allowed_origins,
    })
}

/// Validate that token scopes are allowed by the client's registered scopes.
///
/// The grammar and the subset rules are `happyview-scopes`', which is pinned to
/// the reference implementation. What is HappyView's is resolving `include:`
/// scopes, because only HappyView has a lexicon registry to resolve them with.
///
/// Rules:
/// - `atproto` must be present in the token scopes and is always allowed
/// - every other token scope must be fully covered by the client's registered
///   scopes, *including* the actions it grants — a token asking for create,
///   update and delete is not satisfied by a client registered for create alone
/// - `include:X` client scopes expand to the permissions declared by permission
///   set lexicon `X`, subject to that set's authority containment
pub async fn validate_scopes(
    token_scopes: &str,
    client_scopes: &str,
    lexicons: &crate::lexicon::LexiconRegistry,
) -> Result<(), AppError> {
    let token_list = happyview_scopes::parse_scope_list(token_scopes);

    if !token_list.iter().any(|s| s == "atproto") {
        return Err(AppError::BadRequest(
            "token must include the 'atproto' scope".into(),
        ));
    }

    let mut effective: Vec<String> = Vec::new();
    for scope in happyview_scopes::parse_scope_list(client_scopes) {
        if scope.starts_with("include:") {
            expand_permission_set(&scope, lexicons, &mut effective).await;
        }
        effective.push(scope);
    }

    let client = happyview_scopes::ScopePermissions::from_scopes(effective);

    for scope in &token_list {
        if !client.covers_scope(scope) {
            return Err(AppError::BadRequest(format!(
                "scope '{scope}' is not allowed for this client"
            )));
        }
    }

    Ok(())
}

/// Expand an `include:<nsid>` scope into the permissions its lexicon declares.
///
/// Resolution is the only part that is HappyView's: fetch the lexicon, hand the
/// document to the shared crate, and take back the permissions it yields. The
/// crate applies the rules that make this safe — a permission set may only
/// grant NSIDs under its own authority, may not pin a concrete `aud`, and its
/// `rpc` permissions need an audience to be expressible at all.
///
/// A missing or malformed set contributes nothing rather than failing the
/// whole validation, matching the reference, which skips what it cannot use.
async fn expand_permission_set(
    scope: &str,
    lexicons: &crate::lexicon::LexiconRegistry,
    out: &mut Vec<String>,
) {
    let Some(include) = happyview_scopes::IncludeScope::parse(scope) else {
        tracing::warn!(%scope, "malformed include: scope");
        return;
    };

    let Some(lexicon) = lexicons.get(&include.nsid).await else {
        tracing::warn!(nsid = %include.nsid, "permission set lexicon not found in registry");
        return;
    };

    let Some(permissions) = lexicon
        .raw
        .get("defs")
        .and_then(|d| d.get("main"))
        .and_then(|m| m.get("permissions"))
        .and_then(|p| p.as_array())
    else {
        return;
    };

    let set = happyview_scopes::LexPermissionSet {
        permissions: permissions.iter().filter_map(lex_permission).collect(),
    };

    for permission in include.expand(&set) {
        match permission {
            happyview_scopes::IncludedPermission::Repo(p) => {
                for collection in &p.collection {
                    for action in &p.action {
                        out.push(format!("repo:{collection}?action={}", action.as_str()));
                    }
                }
            }
            happyview_scopes::IncludedPermission::Rpc(p) => {
                for lxm in &p.lxm {
                    // `aud` is not optional in the grammar. Emitting a bare
                    // `rpc:<lxm>` here, as this used to, produced a scope string
                    // the reference rejects outright — so an `include:` set's
                    // rpc permissions never actually matched anything.
                    out.push(format!("rpc:{lxm}?aud={}", urlencoding::encode(&p.aud)));
                }
            }
        }
    }
}

/// Convert one lexicon `permission` entry into the crate's representation,
/// preserving scalar/list arity — the reference treats a scalar supplied where
/// a list belongs as invalidating the permission rather than coercing it.
fn lex_permission(value: &serde_json::Value) -> Option<happyview_scopes::LexPermission> {
    use happyview_scopes::LexValue;

    let obj = value.as_object()?;
    let resource = obj.get("resource")?.as_str()?.to_string();

    let params = obj
        .iter()
        .filter(|(k, _)| k.as_str() != "resource" && k.as_str() != "type")
        .filter_map(|(k, v)| {
            let value = match v {
                serde_json::Value::Array(items) => LexValue::List(
                    items
                        .iter()
                        .map(|i| i.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()?,
                ),
                serde_json::Value::Bool(b) => LexValue::Bool(*b),
                serde_json::Value::String(s) => LexValue::Scalar(s.clone()),
                // Anything else cannot appear in a valid permission; keep the
                // key so the crate's unknown-key check still rejects it.
                _ => LexValue::Scalar(String::new()),
            };
            Some((k.clone(), value))
        })
        .collect();

    Some(happyview_scopes::LexPermission { resource, params })
}

/// Verify a PKCE challenge against a verifier.
pub fn verify_pkce(challenge: &str, verifier: &str) -> bool {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let hash = Sha256::digest(verifier.as_bytes());
    let computed = URL_SAFE_NO_PAD.encode(hash);
    crate::constant_time::ct_eq_str(&computed, challenge)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_registry() -> crate::lexicon::LexiconRegistry {
        crate::lexicon::LexiconRegistry::new()
    }

    #[tokio::test]
    async fn validate_scopes_requires_atproto() {
        let reg = empty_registry();
        let result =
            validate_scopes("transition:generic", "atproto transition:generic", &reg).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn validate_scopes_atproto_only_always_passes() {
        let reg = empty_registry();
        let result = validate_scopes("atproto", "com.example.whatever", &reg).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn validate_scopes_subset_passes() {
        let reg = empty_registry();
        let result = validate_scopes(
            "atproto com.example.basic",
            "atproto com.example.basic com.example.advanced",
            &reg,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn validate_scopes_excess_scope_fails() {
        let reg = empty_registry();
        let result = validate_scopes(
            "atproto com.example.basic com.example.advanced",
            "atproto com.example.basic",
            &reg,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn validate_scopes_transition_generic_requires_registration() {
        let reg = empty_registry();
        let result = validate_scopes("atproto transition:generic", "atproto", &reg).await;
        assert!(result.is_err());

        let result = validate_scopes(
            "atproto transition:generic",
            "atproto transition:generic",
            &reg,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn validate_scopes_expands_include_permission_set() {
        let reg = empty_registry();
        let raw = serde_json::json!({
            "lexicon": 1,
            "id": "com.example.authBasic",
            "defs": {
                "main": {
                    "type": "permission-set",
                    "permissions": [
                        {
                            "type": "permission",
                            "resource": "rpc",
                            "aud": "*",
                            "lxm": ["com.example.getProfile", "com.example.putProfile"]
                        },
                        {
                            "type": "permission",
                            "resource": "repo",
                            "collection": ["com.example.profile"]
                        }
                    ]
                }
            }
        });
        let parsed = crate::lexicon::ParsedLexicon::parse(
            raw,
            1,
            None,
            crate::lexicon::ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        reg.upsert(parsed).await;

        let result = validate_scopes(
            "atproto rpc:com.example.getProfile?aud=* repo:com.example.profile?action=create",
            "atproto include:com.example.authBasic",
            &reg,
        )
        .await;
        assert!(result.is_ok(), "{result:?}");

        let result = validate_scopes(
            "atproto rpc:com.example.notAllowed?aud=*",
            "atproto include:com.example.authBasic",
            &reg,
        )
        .await;
        assert!(result.is_err());
    }

    /// A permission set may only grant NSIDs under its own authority group.
    ///
    /// Without this, publishing a permission-set lexicon would be enough to
    /// vouch for someone else's collections. This check did not exist before
    /// the shared crate; it is the reason adopting it is a behaviour change.
    #[tokio::test]
    async fn include_cannot_grant_another_authoritys_collection() {
        let reg = empty_registry();
        let raw = serde_json::json!({
            "lexicon": 1,
            "id": "com.example.authBasic",
            "defs": {
                "main": {
                    "type": "permission-set",
                    "permissions": [
                        {
                            "type": "permission",
                            "resource": "repo",
                            "collection": ["com.example.profile"]
                        },
                        {
                            "type": "permission",
                            "resource": "repo",
                            "collection": ["app.bsky.feed.post"]
                        }
                    ]
                }
            }
        });
        let parsed = crate::lexicon::ParsedLexicon::parse(
            raw,
            1,
            None,
            crate::lexicon::ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        reg.upsert(parsed).await;

        // Its own authority: granted.
        assert!(
            validate_scopes(
                "atproto repo:com.example.profile?action=create",
                "atproto include:com.example.authBasic",
                &reg,
            )
            .await
            .is_ok()
        );

        // Someone else's: dropped during expansion, so never granted.
        assert!(
            validate_scopes(
                "atproto repo:app.bsky.feed.post?action=create",
                "atproto include:com.example.authBasic",
                &reg,
            )
            .await
            .is_err()
        );
    }

    /// Containment is all-or-nothing *per permission entry*, not per NSID: one
    /// entry listing a foreign collection alongside its own grants neither.
    /// Splitting them into separate entries is what keeps the local one.
    #[tokio::test]
    async fn include_drops_a_whole_entry_that_reaches_outside_its_authority() {
        let reg = empty_registry();
        let raw = serde_json::json!({
            "lexicon": 1,
            "id": "com.example.authBasic",
            "defs": {
                "main": {
                    "type": "permission-set",
                    "permissions": [
                        {
                            "type": "permission",
                            "resource": "repo",
                            "collection": ["com.example.profile", "app.bsky.feed.post"]
                        }
                    ]
                }
            }
        });
        let parsed = crate::lexicon::ParsedLexicon::parse(
            raw,
            1,
            None,
            crate::lexicon::ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        reg.upsert(parsed).await;

        for scope in [
            "atproto repo:com.example.profile?action=create",
            "atproto repo:app.bsky.feed.post?action=create",
        ] {
            assert!(
                validate_scopes(scope, "atproto include:com.example.authBasic", &reg)
                    .await
                    .is_err(),
                "{scope} should not be granted by a mixed-authority entry"
            );
        }
    }

    /// An `rpc` permission with no audience is not expressible, so a permission
    /// set declaring one expands to nothing. This used to emit a bare
    /// `rpc:<lxm>`, which the grammar rejects — meaning it matched nothing
    /// anyway, just less visibly.
    #[tokio::test]
    async fn include_rpc_without_an_audience_grants_nothing() {
        let reg = empty_registry();
        let raw = serde_json::json!({
            "lexicon": 1,
            "id": "com.example.authBasic",
            "defs": {
                "main": {
                    "type": "permission-set",
                    "permissions": [
                        {
                            "type": "permission",
                            "resource": "rpc",
                            "lxm": ["com.example.getProfile"]
                        }
                    ]
                }
            }
        });
        let parsed = crate::lexicon::ParsedLexicon::parse(
            raw,
            1,
            None,
            crate::lexicon::ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        reg.upsert(parsed).await;

        assert!(
            validate_scopes(
                "atproto rpc:com.example.getProfile?aud=*",
                "atproto include:com.example.authBasic",
                &reg,
            )
            .await
            .is_err()
        );
    }

    /// Subsetting is per-action: a client registered for `create` alone does
    /// not satisfy a token asking for every action on the same collection.
    #[tokio::test]
    async fn validate_scopes_subsetting_is_per_action() {
        let reg = empty_registry();

        assert!(
            validate_scopes(
                "atproto repo:com.example.post?action=create",
                "atproto repo:com.example.post?action=create&action=update",
                &reg,
            )
            .await
            .is_ok()
        );

        // The bare form grants all three, which `?action=create` does not cover.
        assert!(
            validate_scopes(
                "atproto repo:com.example.post",
                "atproto repo:com.example.post?action=create",
                &reg,
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn validate_scopes_repo_collection_allowed_with_transition_generic() {
        let reg = empty_registry();
        let result = validate_scopes(
            "atproto transition:generic repo?collection=com.example.profile&collection=com.example.post",
            "atproto transition:generic",
            &reg,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn validate_scopes_repo_collection_allowed_with_expanded_permissions() {
        let reg = empty_registry();
        let raw = serde_json::json!({
            "lexicon": 1,
            "id": "com.example.authBasic",
            "defs": {
                "main": {
                    "type": "permission-set",
                    "permissions": [
                        {
                            "type": "permission",
                            "resource": "repo",
                            "collection": ["com.example.profile", "com.example.post"]
                        }
                    ]
                }
            }
        });
        let parsed = crate::lexicon::ParsedLexicon::parse(
            raw,
            1,
            None,
            crate::lexicon::ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        reg.upsert(parsed).await;

        let result = validate_scopes(
            "atproto repo?collection=com.example.profile&collection=com.example.post",
            "atproto include:com.example.authBasic",
            &reg,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn validate_scopes_repo_collection_rejected_without_permission() {
        let reg = empty_registry();
        let result = validate_scopes(
            "atproto repo?collection=com.example.profile",
            "atproto",
            &reg,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn validate_scopes_repo_collection_rejected_partial_match() {
        let reg = empty_registry();
        let raw = serde_json::json!({
            "lexicon": 1,
            "id": "com.example.authBasic",
            "defs": {
                "main": {
                    "type": "permission-set",
                    "permissions": [
                        {
                            "type": "permission",
                            "resource": "repo",
                            "collection": ["com.example.profile"]
                        }
                    ]
                }
            }
        });
        let parsed = crate::lexicon::ParsedLexicon::parse(
            raw,
            1,
            None,
            crate::lexicon::ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        reg.upsert(parsed).await;

        let result = validate_scopes(
            "atproto repo?collection=com.example.profile&collection=com.example.secret",
            "atproto include:com.example.authBasic",
            &reg,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn validate_scopes_bare_repo_collection_allowed_with_expanded_permissions() {
        let reg = empty_registry();
        let raw = serde_json::json!({
            "lexicon": 1,
            "id": "com.example.authBasic",
            "defs": {
                "main": {
                    "type": "permission-set",
                    "permissions": [
                        {
                            "type": "permission",
                            "resource": "repo",
                            "collection": ["com.example.profile", "com.example.post"]
                        }
                    ]
                }
            }
        });
        let parsed = crate::lexicon::ParsedLexicon::parse(
            raw,
            1,
            None,
            crate::lexicon::ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        reg.upsert(parsed).await;

        let result = validate_scopes(
            "atproto repo:com.example.profile",
            "atproto include:com.example.authBasic",
            &reg,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn validate_scopes_bare_repo_collection_rejected_without_permission() {
        let reg = empty_registry();
        let result = validate_scopes("atproto repo:com.example.secret", "atproto", &reg).await;
        assert!(result.is_err());
    }

    #[test]
    fn verify_pkce_valid() {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;

        let verifier = "test-verifier-string-12345678901234567890";
        let hash = sha2::Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(hash);

        assert!(verify_pkce(&challenge, verifier));
    }

    #[test]
    fn verify_pkce_invalid() {
        assert!(!verify_pkce("wrong-challenge", "some-verifier"));
    }
}
