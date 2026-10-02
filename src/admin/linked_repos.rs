use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;

use crate::AppState;
use crate::error::AppError;
use crate::linked_repos::{db, scope};

use super::auth::UserAuth;
use super::permissions::Permission;

#[derive(Deserialize)]
pub struct CreateLinkedRepoBody {
    pub handle: Option<String>,
    pub reason: Option<String>,
    pub scopes: String,
}

fn validate_scopes(input: &str) -> Result<String, AppError> {
    let mut parsed = scope::parse(input);
    if parsed.is_empty() {
        return Err(AppError::BadRequest("scopes must not be empty".into()));
    }
    for s in &parsed {
        scope::validate(s).map_err(|e| AppError::BadRequest(format!("invalid scope {s}: {e}")))?;
    }
    if !parsed.iter().any(|s| s == "atproto") {
        parsed.insert(0, "atproto".to_string());
    }
    Ok(parsed.join(" "))
}

pub async fn list_linked_repos(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::LinkedReposRead).await?;
    let grants = db::list(&state).await?;
    Ok(Json(serde_json::json!({ "linked_repos": grants })))
}

pub async fn create_linked_repo(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<CreateLinkedRepoBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::LinkedReposCreate).await?;

    let scopes = validate_scopes(&body.scopes)?;

    let (did, handle) = match body
        .handle
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(input) => {
            let resolved = crate::linked_repos::flow::resolve_identifier(&state, input).await?;
            (Some(resolved.did), resolved.handle)
        }
        None => (None, None),
    };

    if let Some(ref did) = did
        && db::get_by_did(&state, did).await?.is_some()
    {
        return Err(AppError::Conflict("this repo is already linked".into()));
    }

    let grant = db::create(
        &state,
        did.as_deref(),
        handle.as_deref(),
        body.reason.as_deref(),
        &scopes,
        &auth.did,
    )
    .await?;

    Ok(Json(serde_json::to_value(grant).unwrap()))
}

pub async fn authorize_linked_repo(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::LinkedReposCreate).await?;

    let grant = db::get(&state, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("linked repo not found".into()))?;

    let identifier = grant
        .handle
        .clone()
        .or_else(|| grant.did.clone())
        .ok_or_else(|| {
            AppError::BadRequest(
                "this grant is open — use an invite link, or recreate it with a handle".into(),
            )
        })?;

    let url = crate::linked_repos::flow::start_authorization(
        &state,
        &grant,
        &identifier,
        crate::linked_repos::flow::AuthOrigin::Admin,
    )
    .await?;
    Ok(Json(serde_json::json!({ "authorize_url": url })))
}

#[derive(Deserialize, Default)]
pub struct InviteBody {
    pub expires_in: Option<i64>,
}

pub async fn invite_linked_repo(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::LinkedReposCreate).await?;

    db::get(&state, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("linked repo not found".into()))?;

    let expires_in = if body.is_empty() {
        None
    } else {
        serde_json::from_slice::<InviteBody>(&body)
            .map_err(|e| AppError::BadRequest(format!("invalid request body: {e}")))?
            .expires_in
    };
    let invite =
        crate::linked_repos::flow::mint_invite_with_expiry(&state, &id, expires_in).await?;
    let base = state.config.effective_public_url();
    let invite_url = format!(
        "{}/auth/linked-repo/start?token={}",
        base.trim_end_matches('/'),
        invite.token
    );

    Ok(Json(serde_json::json!({
        "invite_url": invite_url,
        "expires_at": invite.expires_at,
    })))
}

pub async fn list_linked_repo_invites(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::LinkedReposRead).await?;

    db::get(&state, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("linked repo not found".into()))?;

    let invites = crate::linked_repos::flow::list_invites(&state, &id).await?;
    Ok(Json(serde_json::json!({ "invites": invites })))
}

pub async fn revoke_linked_repo_invite(
    State(state): State<AppState>,
    auth: UserAuth,
    Path((id, invite_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::LinkedReposCreate).await?;

    db::get(&state, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("linked repo not found".into()))?;

    if !crate::linked_repos::flow::revoke_invite(&state, &id, &invite_id).await? {
        return Err(AppError::NotFound(
            "invite not found — it may have been used, revoked, or expired".into(),
        ));
    }

    Ok(Json(serde_json::json!({ "revoked": true })))
}

pub async fn delete_linked_repo(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::LinkedReposDelete).await?;

    if !db::delete(&state, &id).await? {
        return Err(AppError::NotFound("linked repo not found".into()));
    }

    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_scopes_adds_atproto_when_absent() {
        let out = validate_scopes("repo:com.example.note?action=create").unwrap();
        assert_eq!(out, "atproto repo:com.example.note?action=create");
    }

    #[test]
    fn validate_scopes_does_not_duplicate_atproto() {
        let out = validate_scopes("atproto repo:com.example.note").unwrap();
        assert_eq!(out, "atproto repo:com.example.note");
        assert_eq!(out.matches("atproto").count(), 1);
    }

    #[test]
    fn validate_scopes_keeps_atproto_wherever_the_caller_put_it() {
        let out = validate_scopes("repo:com.example.note atproto").unwrap();
        assert_eq!(out, "repo:com.example.note atproto");
    }

    #[test]
    fn validate_scopes_rejects_empty_input() {
        assert!(validate_scopes("   \n ").is_err());
    }

    #[test]
    fn validate_scopes_rejects_a_malformed_token() {
        assert!(validate_scopes("repo:com.example.note?action=frobnicate").is_err());
    }
}
