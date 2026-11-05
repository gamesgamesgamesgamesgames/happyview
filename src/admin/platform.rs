//! Endpoints reserved for the managed-hosting platform principal
//! (`PLATFORM_API_KEY_HASH`). Nothing here is reachable by users or their keys.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};

use super::auth::AllowPlatform;
use super::permissions::Permission;

#[derive(Deserialize)]
pub(super) struct SetSuperUserBody {
    did: String,
}

#[derive(Serialize)]
pub(super) struct SetSuperUserResponse {
    user_id: String,
    did: String,
}

/// PUT /admin/platform/super-user
///
/// Make `did` the instance's only super user, creating the user if needed.
/// Called by the provisioner before an instance is publicly routable, which
/// closes the first-login bootstrap race; also used for ownership transfer.
/// Idempotent.
pub(super) async fn set_super_user(
    State(state): State<AppState>,
    AllowPlatform(auth): AllowPlatform,
    Json(body): Json<SetSuperUserBody>,
) -> Result<Json<SetSuperUserResponse>, AppError> {
    if !auth.is_platform {
        return Err(AppError::Forbidden(
            "only the platform principal can set the super user".into(),
        ));
    }

    let did = body.did.trim().to_string();
    if !did.starts_with("did:") {
        return Err(AppError::BadRequest("did must be a DID".into()));
    }

    let backend = state.db_backend;
    let user_id = transfer_super_user(&state.db, backend, &did, &now_rfc3339())
        .await
        .map_err(|e| AppError::Internal(format!("failed to set the super user: {e}")))?;

    log_event(
        &state.db,
        EventLog {
            event_type: "user.super_set_by_platform".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(did.clone()),
            detail: serde_json::json!({ "user_id": user_id }),
        },
        backend,
    )
    .await;

    Ok(Json(SetSuperUserResponse { user_id, did }))
}

/// Make `did` the only super user, with every permission, creating its user if
/// needed. Returns the user's id.
///
/// One transaction, so a failure part-way through can never leave zero super
/// users or two. It takes the write lock as it begins: a deferred transaction
/// on SQLite reads its snapshot at the first `SELECT`, and a write committed
/// by another connection before this one's first write then fails that write
/// with `SQLITE_BUSY_SNAPSHOT` at once, `busy_timeout` notwithstanding, which
/// a freshly booted instance's own writes can do. It does not guard against
/// concurrent calls on Postgres (READ COMMITTED), so callers must not issue
/// them; the platform calls this sequentially.
async fn transfer_super_user(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    did: &str,
    now: &str,
) -> Result<String, sqlx::Error> {
    let mut tx = crate::db::begin_write(db, backend).await?;

    let existing: Option<(String,)> = crate::db::query_as(&adapt_sql(
        "SELECT id FROM happyview_users WHERE did = ?",
        backend,
    ))
    .bind(did)
    .fetch_optional(&mut *tx)
    .await?;

    let user_id = match existing {
        Some((id,)) => {
            crate::db::query(&adapt_sql(
                "UPDATE happyview_users SET is_super = ? WHERE id = ?",
                backend,
            ))
            .bind(1_i32)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
            id
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            crate::db::query(&adapt_sql(
                "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, ?, ?)",
                backend,
            ))
            .bind(&id)
            .bind(did)
            .bind(1_i32)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            id
        }
    };

    crate::db::query(&adapt_sql(
        "UPDATE happyview_users SET is_super = ? WHERE id <> ?",
        backend,
    ))
    .bind(0_i32)
    .bind(&user_id)
    .execute(&mut *tx)
    .await?;

    let perm_sql = adapt_sql(
        "INSERT INTO happyview_user_permissions (user_id, permission, granted_at) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
        backend,
    );
    for perm in Permission::all() {
        crate::db::query(&perm_sql)
            .bind(&user_id)
            .bind(perm.as_str())
            .bind(now)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    Ok(user_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A write another connection commits while a transfer waits for the
    /// lock: the transfer still succeeds, rather than failing with
    /// `SQLITE_BUSY_SNAPSHOT` as a deferred transaction that had already read
    /// would.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_transfer_survives_a_write_committed_while_it_waits() {
        let path = std::env::temp_dir().join(format!("hv-super-user-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = crate::db::connect(&url, DatabaseBackend::Sqlite).await;

        // Another connection holds the write lock and has written, as a
        // booting instance's own work does.
        let mut other = crate::db::begin_write(&pool, DatabaseBackend::Sqlite)
            .await
            .expect("take the write lock");
        crate::db::query(
            "INSERT INTO happyview_instance_settings (key, value) VALUES ('boot', 'x')",
        )
        .execute(&mut *other)
        .await
        .expect("write under the lock");

        let transfer = tokio::spawn({
            let pool = pool.clone();
            async move {
                transfer_super_user(
                    &pool,
                    DatabaseBackend::Sqlite,
                    "did:plc:owner",
                    &now_rfc3339(),
                )
                .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        other.commit().await.expect("release the write lock");

        let user_id = transfer
            .await
            .expect("the transfer task")
            .expect("the transfer survives the other write");
        let (is_super,): (i32,) =
            crate::db::query_as("SELECT is_super FROM happyview_users WHERE id = ?")
                .bind(&user_id)
                .fetch_one(&pool)
                .await
                .expect("read the user back");
        assert_eq!(is_super, 1);

        pool.close().await;
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}
