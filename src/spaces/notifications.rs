use crate::db::{DatabaseBackend, now_rfc3339};
use crate::error::AppError;
use crate::spaces::db;
use crate::spaces::types::NotifyRegistration;
use uuid::Uuid;

const NOTIFY_REGISTRATION_TTL_SECS: u64 = 24 * 60 * 60; // 24 hours

pub async fn register(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    service_did: &str,
    endpoint: &str,
    registered_by: &str,
) -> Result<String, AppError> {
    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let expires_at = {
        let expiry = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + NOTIFY_REGISTRATION_TTL_SECS;
        chrono::DateTime::from_timestamp(expiry as i64, 0)
            .unwrap()
            .to_rfc3339()
    };
    let reg = NotifyRegistration {
        id: id.clone(),
        space_id: space_id.to_string(),
        author_did: Some(service_did.to_string()),
        endpoint: endpoint.to_string(),
        registered_by: registered_by.to_string(),
        expires_at,
        created_at: now,
    };
    db::register_notify(pool, backend, &reg).await?;
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
pub async fn dispatch_write_notification(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    http: &reqwest::Client,
    space_id: &str,
    author_did: &str,
    collection: &str,
    rkey: &str,
    cid: Option<&str>,
) -> Result<(), AppError> {
    let registrations =
        db::list_notify_registrations(pool, backend, space_id, Some(author_did)).await?;
    // Also include space-wide registrations (no author_did filter)
    let space_wide = db::list_notify_registrations(pool, backend, space_id, None).await?;

    let all: Vec<&NotifyRegistration> = registrations
        .iter()
        .chain(space_wide.iter().filter(|r| r.author_did.is_none()))
        .collect();

    let payload = serde_json::json!({
        "space": space_id,
        "did": author_did,
        "collection": collection,
        "rkey": rkey,
        "cid": cid,
    });

    for reg in all {
        let _ = http.post(&reg.endpoint).json(&payload).send().await;
    }

    Ok(())
}

pub async fn dispatch_space_deleted(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    http: &reqwest::Client,
    space_id: &str,
) -> Result<(), AppError> {
    let registrations = db::list_notify_registrations(pool, backend, space_id, None).await?;

    let payload = serde_json::json!({ "space": space_id });

    for reg in &registrations {
        let _ = http.post(&reg.endpoint).json(&payload).send().await;
    }

    Ok(())
}
