//! `/admin/storage` — record body compression.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql};
use crate::error::AppError;
use crate::record_store::compression::{self, Compression, TrainError};

use super::auth::UserAuth;
use super::permissions::Permission;

/// Held while a retrain runs: one at a time per process. Each training reads
/// up to 32 MiB of samples and every success keeps another dictionary for
/// good, so a double-clicked button or a retry loop must not start several.
static RETRAIN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The retrain lock, or `409 Conflict` while another retrain holds it.
fn begin_retrain() -> Result<tokio::sync::MutexGuard<'static, ()>, AppError> {
    RETRAIN.try_lock().map_err(|_| {
        AppError::Conflict(
            "a dictionary is already being trained; try again when it finishes".into(),
        )
    })
}

#[derive(Deserialize)]
pub(super) struct RetrainBody {
    collection: String,
}

/// `POST /admin/storage/retrain` — train a new zstd dictionary for one
/// collection from its newest records. New writes use it at once; rows
/// already compressed keep the dictionary they were written with, which is
/// never deleted while they reference it.
pub(super) async fn retrain(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<RetrainBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;
    if state.db_backend != DatabaseBackend::Sqlite {
        return Err(AppError::BadRequest(
            "record compression applies to SQLite instances only".into(),
        ));
    }
    if state.records.bodies().mode() == Compression::None {
        return Err(AppError::BadRequest(
            "record compression is off (RECORD_COMPRESSION=none)".into(),
        ));
    }
    let sql = adapt_sql(
        "SELECT id FROM happyview_collections WHERE nsid = ?",
        state.db_backend,
    );
    let Some((collection_id,)) = crate::db::query_as::<(i64,)>(&sql)
        .bind(&body.collection)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up the collection: {e}")))?
    else {
        return Err(AppError::NotFound(format!(
            "no records in {}",
            body.collection
        )));
    };
    let _training = begin_retrain()?;
    let dict_id = compression::train(
        &state.db,
        state.records.bodies(),
        collection_id,
        compression::DICT_BYTES,
    )
    .await
    .map_err(|e| match e {
        TrainError::NoSamples => AppError::NotFound(format!("no records in {}", body.collection)),
        TrainError::Zstd(message) => AppError::XrpcError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "TrainingFailed",
            message: format!(
                "could not train a dictionary for {}: {message}",
                body.collection
            ),
        },
        other => AppError::Internal(format!("failed to train a dictionary: {other}")),
    })?;
    Ok(Json(
        json!({ "collection": body.collection, "dict_id": dict_id }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_retrain_while_another_runs_is_a_conflict() {
        let first = begin_retrain().expect("the first retrain starts");
        match begin_retrain() {
            Err(AppError::Conflict(message)) => {
                assert!(message.contains("already being trained"), "{message}")
            }
            Err(other) => panic!("expected a conflict, got {other:?}"),
            Ok(_) => panic!("expected a conflict, got the lock"),
        }
        drop(first);
        drop(begin_retrain().expect("free again once the first finishes"));
    }
}
