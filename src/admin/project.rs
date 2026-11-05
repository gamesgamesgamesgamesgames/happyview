//! `PUT` and `GET /admin/project`: the project `hpy deploy` sends.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::project::deploy::{self, DeployError, DeployRequest, bundle, store};

use super::auth::UserAuth;
use super::permissions::Permission;

#[derive(Deserialize)]
pub(super) struct DeployQuery {
    #[serde(default)]
    dry_run: bool,
}

pub(super) async fn put_project(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(query): Query<DeployQuery>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, AppError> {
    auth.require(Permission::ProjectDeploy).await?;
    let bundle = match body {
        Ok(bytes) => bytes,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return Ok(refusal(DeployError::TooLarge));
        }
        Err(rejection) => return Err(AppError::BadRequest(rejection.body_text())),
    };
    let if_match = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok());
    let dry_run = query.dry_run;

    let report = match deploy::deploy(
        &state,
        DeployRequest {
            bundle,
            dry_run,
            if_match,
            deployed_by: &auth.did,
        },
    )
    .await
    {
        Ok(report) => report,
        Err(DeployError::Internal(message)) => return Err(AppError::Internal(message)),
        Err(refused) => return Ok(refusal(refused)),
    };

    if !dry_run {
        log_event(
            &state.db,
            EventLog {
                event_type: "project.deployed".to_string(),
                severity: Severity::Info,
                actor_did: Some(auth.did.clone()),
                subject: Some(report.sha256.clone()),
                detail: json!({
                    "changes": report.changes,
                    "backfills": report.backfills,
                }),
            },
            state.db_backend,
        )
        .await;
    }
    Ok(Json(report).into_response())
}

pub(super) async fn get_project(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Response, AppError> {
    auth.require(Permission::ProjectDeploy).await?;
    if let Some(root) = &state.config.project_root {
        return Ok(refusal(DeployError::FromFile(root.clone())));
    }
    let Some(current) = store::current(&state.db, state.db_backend)
        .await
        .map_err(|e| AppError::Internal(format!("could not read the deployed project: {e}")))?
    else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "NoProject",
                "message": "no project has been deployed to this instance",
            })),
        )
            .into_response());
    };
    let mut body = json!({
        "sha256": current.sha256,
        "deployed_at": current.deployed_at,
        "deployed_by": current.deployed_by,
    });
    let found = deploy::boot_problems();
    if !found.is_empty() {
        body["error"] = json!({
            "message": "the deployed project did not build when the instance started, so none is being served",
            "problems": deploy::problems(&found),
        });
    }
    Ok(Json(body).into_response())
}

/// A deploy refused for a reason the client can act on. `Internal` is never
/// one: it goes out as an `AppError`.
fn refusal(error: DeployError) -> Response {
    let (status, body) = match error {
        DeployError::FromFile(_) => (
            StatusCode::CONFLICT,
            json!({
                "error": "ProjectFromFile",
                "message": "this instance serves the project HAPPYVIEW_PROJECT names, so a deploy cannot replace it; change those files instead",
            }),
        ),
        DeployError::TooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({
                "error": "BundleTooLarge",
                "message": format!(
                    "the bundle is larger than {} MiB",
                    bundle::MAX_COMPRESSED_BYTES / (1024 * 1024)
                ),
            }),
        ),
        DeployError::PreconditionFailed(current) => (
            StatusCode::PRECONDITION_FAILED,
            json!({
                "error": "PreconditionFailed",
                "message": "the deployed project is not the one If-Match names; check what is deployed and deploy again",
                "current": current,
            }),
        ),
        DeployError::Invalid(found) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "error": "InvalidProject",
                "message": "the project did not build, so nothing was applied",
                "problems": deploy::problems(&found),
            }),
        ),
        DeployError::Internal(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "InternalServerError", "message": message}),
        ),
    };
    (status, Json(body)).into_response()
}
