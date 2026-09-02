//! `/admin/settings/telemetry` — consent toggles, payload preview, and manual send.

use axum::Json;
use axum::extract::State;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::telemetry::{assemble, consent, reporter};

use super::auth::UserAuth;
use super::permissions::Permission;

const MAX_CONTACT_LEN: usize = 512;

#[derive(Debug, Default, serde::Deserialize)]
pub struct TelemetryUpdate {
    pub mode: Option<String>,
    pub contact: Option<String>,
    pub lexicon_names: Option<bool>,
    pub lexicon_structure: Option<bool>,
    pub lexicon_documents: Option<bool>,
}

pub fn validate_mode(mode: &str) -> Result<(), AppError> {
    match mode {
        "off" | "manual" | "auto" => Ok(()),
        other => Err(AppError::BadRequest(format!(
            "unknown telemetry mode {other:?}; expected off, manual or auto"
        ))),
    }
}

pub fn validate_contact(contact: &str) -> Result<(), AppError> {
    if contact.len() > MAX_CONTACT_LEN {
        return Err(AppError::BadRequest(format!(
            "contact must be at most {MAX_CONTACT_LEN} characters"
        )));
    }
    Ok(())
}

async fn put_setting(state: &AppState, key: &str, value: &str) -> Result<(), AppError> {
    let sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at)
         VALUES (?, ?, ?)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(key)
        .bind(value)
        .bind(now_rfc3339())
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to save telemetry setting: {e}")))?;
    Ok(())
}

/// `GET /admin/settings/telemetry`
pub(super) async fn get(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let c = consent::load(&state.db, state.db_backend).await;
    Ok(Json(serde_json::json!({
        "mode": c.mode.as_str(),
        "contact": c.contact,
        "lexicon_names": c.lexicon_names,
        "lexicon_structure": c.lexicon_structure,
        "lexicon_documents": c.lexicon_documents,
        "instance_id": c.instance_id,
        "collector_url": state.config.telemetry_collector_url,
    })))
}

/// `PUT /admin/settings/telemetry`
pub(super) async fn update(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<TelemetryUpdate>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    if let Some(mode) = &body.mode {
        validate_mode(mode)?;
    }
    if let Some(contact) = &body.contact {
        validate_contact(contact)?;
    }

    if let Some(mode) = &body.mode {
        put_setting(&state, consent::KEY_MODE, mode).await?;
    }
    if let Some(contact) = &body.contact {
        put_setting(&state, consent::KEY_CONTACT, contact).await?;
    }
    for (value, key) in [
        (body.lexicon_names, consent::KEY_LEXICON_NAMES),
        (body.lexicon_structure, consent::KEY_LEXICON_STRUCTURE),
        (body.lexicon_documents, consent::KEY_LEXICON_DOCUMENTS),
    ] {
        if let Some(v) = value {
            put_setting(&state, key, if v { "true" } else { "false" }).await?;
        }
    }

    consent::ensure_instance_id(&state.db, state.db_backend).await;

    get(State(state), auth).await
}

/// `GET /admin/settings/telemetry/preview`
pub(super) async fn preview(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let mut c = consent::load(&state.db, state.db_backend).await;
    if c.instance_id.is_none() {
        c.instance_id = Some("(generated when you enable telemetry)".to_string());
    }

    let snapshot = assemble::assemble(
        &state.db,
        state.db_backend,
        &state.config.database_url,
        &c,
        &state.telemetry_counters,
    )
    .await
    .ok_or_else(|| AppError::Internal("failed to assemble telemetry preview".to_string()))?;

    serde_json::to_value(snapshot)
        .map(Json)
        .map_err(|e| AppError::Internal(format!("failed to encode preview: {e}")))
}

/// `POST /admin/settings/telemetry/send`
pub(super) async fn send(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let mode = consent::load(&state.db, state.db_backend).await.mode;
    if !mode.reports() {
        return Err(AppError::BadRequest(
            "telemetry is disabled; enable manual or automatic reporting first".to_string(),
        ));
    }

    match reporter::report_once(&state).await {
        Ok(benchmarks) => Ok(Json(
            serde_json::json!({ "sent": true, "benchmarks": benchmarks }),
        )),
        Err(e) => Err(AppError::Internal(format!("telemetry send failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_omitted_field_means_unchanged() {
        let update: TelemetryUpdate = serde_json::from_str(r#"{"mode":"auto"}"#).unwrap();
        assert_eq!(update.mode.as_deref(), Some("auto"));
        assert!(update.lexicon_names.is_none());
        assert!(update.contact.is_none());
    }

    #[test]
    fn an_explicit_false_is_distinguishable_from_absent() {
        let update: TelemetryUpdate = serde_json::from_str(r#"{"lexicon_names":false}"#).unwrap();
        assert_eq!(update.lexicon_names, Some(false));
    }

    #[test]
    fn rejects_a_mode_it_does_not_recognise() {
        assert!(validate_mode("auto").is_ok());
        assert!(validate_mode("manual").is_ok());
        assert!(validate_mode("off").is_ok());
        assert!(validate_mode("enabled").is_err());
        assert!(validate_mode("").is_err());
    }

    #[test]
    fn rejects_an_overlong_contact_string() {
        assert!(validate_contact("tre@trezy.com").is_ok());
        assert!(validate_contact(&"x".repeat(513)).is_err());
    }
}
