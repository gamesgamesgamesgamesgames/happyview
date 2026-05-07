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
        "SELECT id, client_key, client_type, scopes, allowed_origins, client_secret_hash FROM api_clients WHERE client_key = ? AND is_active = 1",
        backend,
    );

    let row: Option<(String, String, String, String, Option<String>, String)> =
        sqlx::query_as(&sql)
            .bind(client_key)
            .fetch_optional(pool)
            .await
            .map_err(|e| AppError::Internal(format!("client lookup failed: {e}")))?;

    let (id, key, client_type, scopes, origins_json, stored_hash) =
        row.ok_or_else(|| AppError::Auth("invalid client credentials".into()))?;

    if stored_hash != secret_hash {
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
        "SELECT id, client_key, client_type, scopes, allowed_origins FROM api_clients WHERE client_key = ? AND is_active = 1",
        backend,
    );

    let row: Option<(String, String, String, String, Option<String>)> = sqlx::query_as(&sql)
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
        "SELECT id, client_key, client_type, scopes, allowed_origins FROM api_clients WHERE client_key = ? AND is_active = 1",
        backend,
    );

    let row: Option<(String, String, String, String, Option<String>)> = sqlx::query_as(&sql)
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
/// Rules:
/// - `atproto` must be present in token scopes (always implicitly allowed)
/// - Every non-`atproto` scope in the token must appear in the client's registered scopes
/// - `include:X` client scopes are expanded by looking up the permission set
///   lexicon `X` and extracting its `rpc:` and `repo:` permissions
/// - `repo?collection=X&collection=Y` scopes (PDS-granted) are allowed if the
///   client has `transition:generic` or has collection-level permissions from
///   expanded `include:` scopes
pub async fn validate_scopes(
    token_scopes: &str,
    client_scopes: &str,
    lexicons: &crate::lexicon::LexiconRegistry,
) -> Result<(), AppError> {
    let token_set: std::collections::HashSet<&str> = token_scopes.split_whitespace().collect();
    let mut client_set: std::collections::HashSet<String> = std::collections::HashSet::new();

    for scope in client_scopes.split_whitespace() {
        if let Some(perm_set_id) = scope.strip_prefix("include:") {
            expand_permission_set(perm_set_id, lexicons, &mut client_set).await;
        }
        client_set.insert(scope.to_string());
    }

    if !token_set.contains("atproto") {
        return Err(AppError::BadRequest(
            "token must include the 'atproto' scope".into(),
        ));
    }

    let has_generic = client_set.contains("transition:generic");

    for scope in &token_set {
        if *scope == "atproto" {
            continue;
        }
        if client_set.contains(*scope) {
            continue;
        }

        // The PDS grants `repo?collection=X&collection=Y` to restrict which
        // collections the token can access.  Allow if the client has broad
        // access (`transition:generic`) or has matching collection-level
        // permissions from expanded `include:` scopes.
        if let Some(query) = scope.strip_prefix("repo?") {
            if has_generic {
                continue;
            }
            let all_allowed = query.split('&').all(|param| {
                let Some(col) = param.strip_prefix("collection=") else {
                    return true;
                };
                let prefix = format!("repo:{}?", col);
                client_set.iter().any(|cs| cs.starts_with(&prefix))
            });
            if all_allowed {
                continue;
            }
        }

        return Err(AppError::BadRequest(format!(
            "scope '{}' is not allowed for this client",
            scope
        )));
    }

    Ok(())
}

/// Expand a permission set lexicon into individual `rpc:` and `repo:` scopes.
async fn expand_permission_set(
    nsid: &str,
    lexicons: &crate::lexicon::LexiconRegistry,
    out: &mut std::collections::HashSet<String>,
) {
    let lexicon = match lexicons.get(nsid).await {
        Some(l) => l,
        None => {
            tracing::warn!(nsid = %nsid, "permission set lexicon not found in registry");
            return;
        }
    };

    let permissions = match lexicon
        .raw
        .get("defs")
        .and_then(|d| d.get("main"))
        .and_then(|m| m.get("permissions"))
        .and_then(|p| p.as_array())
    {
        Some(p) => p,
        None => return,
    };

    for perm in permissions {
        let resource = perm.get("resource").and_then(|r| r.as_str()).unwrap_or("");
        match resource {
            "rpc" => {
                if let Some(lxms) = perm.get("lxm").and_then(|l| l.as_array()) {
                    for lxm in lxms {
                        if let Some(s) = lxm.as_str() {
                            out.insert(format!("rpc:{s}"));
                        }
                    }
                }
            }
            "repo" => {
                if let Some(collections) = perm.get("collection").and_then(|c| c.as_array()) {
                    for col in collections {
                        if let Some(s) = col.as_str() {
                            out.insert(format!("repo:{s}?action=create"));
                            out.insert(format!("repo:{s}?action=update"));
                            out.insert(format!("repo:{s}?action=delete"));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Verify a PKCE challenge against a verifier.
pub fn verify_pkce(challenge: &str, verifier: &str) -> bool {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let hash = Sha256::digest(verifier.as_bytes());
    let computed = URL_SAFE_NO_PAD.encode(hash);
    computed == challenge
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
            "atproto rpc:com.example.getProfile repo:com.example.profile?action=create",
            "atproto include:com.example.authBasic",
            &reg,
        )
        .await;
        assert!(result.is_ok());

        let result = validate_scopes(
            "atproto rpc:com.example.notAllowed",
            "atproto include:com.example.authBasic",
            &reg,
        )
        .await;
        assert!(result.is_err());
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
