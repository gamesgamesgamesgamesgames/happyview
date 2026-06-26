use axum::{
    Json,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::auth::UserAuth;
use super::permissions::Permission;
use crate::AppState;
use crate::db::{adapt_sql, parse_dt};
use crate::error::AppError;

#[derive(Deserialize)]
pub struct EventsQuery {
    pub event_type: Option<String>,
    pub category: Option<String>,
    pub severity: Option<String>,
    pub subject: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct EventResponse {
    pub id: String,
    pub event_type: String,
    pub severity: String,
    pub actor_did: Option<String>,
    pub subject: Option<String>,
    pub detail: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
pub struct EventsListResponse {
    pub events: Vec<EventResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// GET /admin/events — list event logs with optional filters and pagination.
pub(super) async fn list_events(
    auth: UserAuth,
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<EventsListResponse>, AppError> {
    auth.require(Permission::EventsRead).await?;
    let backend = state.db_backend;
    let limit = query.limit.unwrap_or(50).clamp(1, 100);

    let mut sql = String::from(
        "SELECT id, event_type, severity, actor_did, subject, detail, created_at
         FROM happyview_event_logs WHERE 1=1",
    );

    if query.event_type.is_some() {
        sql.push_str(" AND event_type = ?");
    }
    if let Some(ref cat) = query.category {
        let cats: Vec<&str> = cat.split(',').collect();
        let clauses: Vec<String> = cats
            .iter()
            .map(|_| "event_type LIKE ?".to_string())
            .collect();
        sql.push_str(&format!(" AND ({})", clauses.join(" OR ")));
    }
    if let Some(ref sev) = query.severity {
        let count = sev.split(',').count();
        let placeholders: Vec<&str> = (0..count).map(|_| "?").collect();
        sql.push_str(&format!(" AND severity IN ({})", placeholders.join(",")));
    }
    if query.subject.is_some() {
        sql.push_str(" AND subject LIKE ?");
    }
    if query.cursor.is_some() {
        sql.push_str(" AND created_at < ?");
    }

    sql.push_str(" ORDER BY created_at DESC LIMIT ?");

    let sql = adapt_sql(&sql, backend);

    #[allow(clippy::type_complexity)]
    let mut q = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
        ),
    >(&sql);

    if let Some(ref event_type) = query.event_type {
        q = q.bind(event_type);
    }
    if let Some(ref category) = query.category {
        for c in category.split(',') {
            q = q.bind(format!("{c}.%"));
        }
    }
    if let Some(ref severity) = query.severity {
        for s in severity.split(',') {
            q = q.bind(s.to_string());
        }
    }
    if let Some(ref subject) = query.subject {
        q = q.bind(format!("%{subject}%"));
    }
    if let Some(ref cursor) = query.cursor {
        q = q.bind(cursor);
    }
    q = q.bind(limit);

    let rows = q
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query events: {e}")))?;

    let events: Vec<EventResponse> = rows
        .into_iter()
        .map(|row| EventResponse {
            id: row.0,
            event_type: row.1,
            severity: row.2,
            actor_did: row.3,
            subject: row.4,
            detail: serde_json::from_str(&row.5).unwrap_or(Value::Object(Default::default())),
            created_at: parse_dt(&row.6),
        })
        .collect();

    let cursor = if events.len() as i64 >= limit {
        events.last().map(|e| e.created_at.to_rfc3339())
    } else {
        None
    };

    Ok(Json(EventsListResponse { events, cursor }))
}
