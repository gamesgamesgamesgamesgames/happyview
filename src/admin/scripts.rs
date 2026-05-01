//! `/admin/scripts` CRUD — trigger-keyed scripts.
//!
//! Each script row's `id` IS its trigger string (e.g.
//! `record.create:com.example.thing`, `xrpc.query:com.foo.list`,
//! `labeler.apply:_actor`). The dispatcher in [`crate::lua::scripts`]
//! looks up scripts by id at firing time; this admin surface lets
//! operators CRUD those rows.
//!
//! Validation:
//! - On create / patch the body is parsed against the script_type
//!   (lua → [`crate::lua::validate_script`]). Invalid bodies are
//!   rejected at write-time with a 400.
//! - The trigger id is parsed against
//!   [`crate::lua::ParsedTrigger::parse`]; unknown prefixes / invalid
//!   NSIDs are rejected at write-time with a 400.
//!
//! Permissions: `scripts:read` for GETs; `scripts:manage` for the
//! mutating endpoints.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::lua::{ParsedTrigger, ScriptLanguage};

use super::auth::UserAuth;
use super::permissions::Permission;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One row from the `scripts` table — what GET endpoints return.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ScriptResponse {
    /// The trigger id; identifies the row.
    pub id: String,
    pub script_type: String,
    pub body: String,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Body for `POST /admin/scripts` (create or replace by `id`).
#[derive(Debug, Deserialize)]
pub(super) struct UpsertBody {
    pub id: String,
    /// Defaults to `"lua"` server-side if omitted.
    #[serde(default)]
    pub script_type: Option<ScriptLanguage>,
    pub body: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Body for `PATCH /admin/scripts/{id}`. All fields optional.
#[derive(Debug, Deserialize)]
pub(super) struct PatchBody {
    #[serde(default)]
    pub script_type: Option<ScriptLanguage>,
    #[serde(default)]
    pub body: Option<String>,
    /// Set to `Some(None)` to clear via JSON `null`.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub description: Option<Option<String>>,
}

/// Three-state field deserializer: missing → `None`, `null` → `Some(None)`,
/// string → `Some(Some(s))`. Lets PATCH distinguish "leave as-is" from
/// "clear to NULL".
fn deserialize_optional_field<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: Option<String> = Option::deserialize(d)?;
    Ok(Some(v))
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// `GET /admin/scripts` — list all rows. Clients group by trigger family
/// in the UI.
pub(super) async fn list(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<ScriptResponse>>, AppError> {
    auth.require(Permission::ScriptsRead).await?;

    let backend = state.db_backend;
    let sql = adapt_sql(
        "SELECT id, script_type, body, description, created_at, updated_at
         FROM scripts
         ORDER BY id",
        backend,
    );
    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, String, String, Option<String>, String, String)> = sqlx::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list scripts: {e}")))?;

    let scripts: Vec<ScriptResponse> = rows
        .into_iter()
        .map(
            |(id, script_type, body, description, created_at, updated_at)| ScriptResponse {
                id,
                script_type,
                body,
                description,
                created_at,
                updated_at,
            },
        )
        .collect();

    Ok(Json(scripts))
}

/// `GET /admin/scripts/{id}` — fetch one row.
pub(super) async fn get(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<ScriptResponse>, AppError> {
    auth.require(Permission::ScriptsRead).await?;
    fetch_one(&state, &id).await.map(Json)
}

/// `POST /admin/scripts` — create or replace a row by `id`. Returns the
/// upserted row. Status `201 Created` for a new row, `200 OK` for an
/// update.
pub(super) async fn upsert(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<UpsertBody>,
) -> Result<(StatusCode, Json<ScriptResponse>), AppError> {
    auth.require(Permission::ScriptsManage).await?;

    // Validate the trigger id grammar up-front (400 with a clear message).
    let _trigger = ParsedTrigger::parse(&body.id).map_err(AppError::BadRequest)?;

    let script_type = body.script_type.unwrap_or_default();
    validate_body_for_type(&body.body, script_type)?;

    let backend = state.db_backend;
    let now = now_rfc3339();
    let description = body.description.as_deref().filter(|s| !s.is_empty());

    // Distinguish create vs update so we can return 201 vs 200.
    let pre_exists: Option<(String,)> =
        sqlx::query_as(&adapt_sql("SELECT id FROM scripts WHERE id = ?", backend))
            .bind(&body.id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to check script existence: {e}")))?;
    let was_new = pre_exists.is_none();

    let sql = adapt_sql(
        r#"
        INSERT INTO scripts (id, script_type, body, description, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT (id) DO UPDATE SET
            script_type = EXCLUDED.script_type,
            body        = EXCLUDED.body,
            description = EXCLUDED.description,
            updated_at  = EXCLUDED.updated_at
        "#,
        backend,
    );
    sqlx::query(&sql)
        .bind(&body.id)
        .bind(script_type.as_str())
        .bind(&body.body)
        .bind(description)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to upsert script: {e}")))?;

    log_event(
        &state.db,
        EventLog {
            event_type: if was_new {
                "script.created".to_string()
            } else {
                "script.updated".to_string()
            },
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(body.id.clone()),
            detail: serde_json::json!({
                "script_type": script_type.as_str(),
            }),
        },
        backend,
    )
    .await;

    let row = fetch_one(&state, &body.id).await?;
    let status = if was_new {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(row)))
}

/// `PATCH /admin/scripts/{id}` — partial update. At least one of
/// `script_type` / `body` / `description` must be present.
pub(super) async fn patch(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    Json(body): Json<PatchBody>,
) -> Result<Json<ScriptResponse>, AppError> {
    auth.require(Permission::ScriptsManage).await?;

    if body.script_type.is_none() && body.body.is_none() && body.description.is_none() {
        return Err(AppError::BadRequest(
            "patch requires at least one of: script_type, body, description".into(),
        ));
    }
    // Patching a body or script_type? We need a body to validate against
    // the (possibly new) language. Patching script_type alone is
    // ambiguous (we'd be validating the existing body against the new
    // language without re-checking it makes sense), so reject it.
    if body.script_type.is_some() && body.body.is_none() {
        return Err(AppError::BadRequest(
            "patching script_type requires body alongside (so the server can re-validate)".into(),
        ));
    }
    if let Some(ref new_body) = body.body {
        let lang = body.script_type.unwrap_or_default();
        validate_body_for_type(new_body, lang)?;
    }

    // Existence check + fetch current values.
    let existing = fetch_one(&state, &id).await?;

    let backend = state.db_backend;
    let now = now_rfc3339();
    let new_script_type = body
        .script_type
        .map(|s| s.as_str().to_string())
        .unwrap_or(existing.script_type);
    let new_body = body.body.unwrap_or(existing.body);
    let new_description = match body.description {
        Some(desc_opt) => desc_opt,
        None => existing.description,
    };

    let sql = adapt_sql(
        r#"
        UPDATE scripts
           SET script_type = ?,
               body        = ?,
               description = ?,
               updated_at  = ?
         WHERE id = ?
        "#,
        backend,
    );
    sqlx::query(&sql)
        .bind(&new_script_type)
        .bind(&new_body)
        .bind(new_description.as_deref())
        .bind(&now)
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to patch script: {e}")))?;

    log_event(
        &state.db,
        EventLog {
            event_type: "script.updated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id.clone()),
            detail: serde_json::json!({
                "script_type": new_script_type,
            }),
        },
        backend,
    )
    .await;

    let row = fetch_one(&state, &id).await?;
    Ok(Json(row))
}

/// `DELETE /admin/scripts/{id}` — remove a row. 204 on success, 404 if
/// no row matched.
pub(super) async fn delete(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::ScriptsManage).await?;

    let backend = state.db_backend;
    let sql = adapt_sql("DELETE FROM scripts WHERE id = ?", backend);
    let result = sqlx::query(&sql)
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete script: {e}")))?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("script '{id}' not found")));
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "script.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id),
            detail: serde_json::json!({}),
        },
        backend,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Look up a single script row; 404 if missing.
async fn fetch_one(state: &AppState, id: &str) -> Result<ScriptResponse, AppError> {
    let backend = state.db_backend;
    let sql = adapt_sql(
        "SELECT id, script_type, body, description, created_at, updated_at
         FROM scripts WHERE id = ?",
        backend,
    );
    #[allow(clippy::type_complexity)]
    let row: Option<(String, String, String, Option<String>, String, String)> =
        sqlx::query_as(&sql)
            .bind(id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to fetch script: {e}")))?;
    let (id, script_type, body, description, created_at, updated_at) =
        row.ok_or_else(|| AppError::NotFound(format!("script '{id}' not found")))?;
    Ok(ScriptResponse {
        id,
        script_type,
        body,
        description,
        created_at,
        updated_at,
    })
}

/// Validate the script body against its declared language. Rejects
/// invalid bodies with a 400 at write-time.
fn validate_body_for_type(body: &str, lang: ScriptLanguage) -> Result<(), AppError> {
    match lang {
        ScriptLanguage::Lua => crate::lua::validate_script(body).map_err(AppError::BadRequest),
    }
}
