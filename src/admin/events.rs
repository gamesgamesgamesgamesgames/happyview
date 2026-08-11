use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::auth::UserAuth;
use super::permissions::Permission;
use crate::AppState;
use crate::db::{adapt_sql, parse_dt};
use crate::error::AppError;
use crate::event_log::{EventFilter, EventLog, Severity, log_event, normalize_rfc3339};

#[derive(Deserialize)]
pub struct EventsQuery {
    pub event_type: Option<String>,
    pub category: Option<String>,
    pub severity: Option<String>,
    pub subject: Option<String>,
    /// Exclusive upper bound on `created_at`, RFC3339.
    pub before: Option<String>,
    /// Inclusive lower bound on `created_at`, RFC3339.
    pub after: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

/// Build an `EventFilter` from query params, normalising the timestamp bounds.
///
/// A blank or whitespace-only value means "unset" for every field, not "match
/// nothing" — `?event_type=` and `{"event_type": ""}` both become `None` here
/// rather than `Some("")`. This is the single point both `count_events` and
/// `purge_events` build their filter through, so they cannot disagree about
/// what an empty string means.
pub(super) fn filter_from_query(q: &EventsQuery) -> Result<EventFilter, AppError> {
    let text = |v: &Option<String>| -> Option<String> {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let norm = |v: &Option<String>| -> Result<Option<String>, AppError> {
        match text(v) {
            Some(s) => normalize_rfc3339(&s)
                .map(Some)
                .map_err(AppError::BadRequest),
            None => Ok(None),
        }
    };
    Ok(EventFilter {
        event_type: text(&q.event_type),
        category: text(&q.category),
        severity: text(&q.severity),
        subject: text(&q.subject),
        after: norm(&q.after)?,
        before: norm(&q.before)?,
    })
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

    let filter = filter_from_query(&query)?;
    let (frag, binds) = filter.build();

    let mut sql = format!(
        "SELECT id, event_type, severity, actor_did, subject, detail, created_at
         FROM happyview_event_logs WHERE 1=1{frag}"
    );
    if query.cursor.is_some() {
        sql.push_str(" AND created_at < ?");
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT ?");

    let sql = adapt_sql(&sql, backend);

    #[allow(clippy::type_complexity)]
    let mut q = crate::db::query_as::<(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
    )>(&sql);

    for bind in &binds {
        q = q.bind(bind);
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

#[derive(Deserialize)]
pub struct PurgeBody {
    pub event_type: Option<String>,
    pub category: Option<String>,
    pub severity: Option<String>,
    pub subject: Option<String>,
    /// Exclusive upper bound on `created_at`, RFC3339.
    pub before: Option<String>,
    /// Inclusive lower bound on `created_at`, RFC3339.
    pub after: Option<String>,
}

/// GET /admin/events/count — how many events match a filter.
///
/// The dashboard calls this before offering to purge. It shares
/// `filter_from_query` with the list endpoint and the purge job, so the number
/// shown is the number deleted.
///
/// Reuses `EventsQuery` for convenience, but `cursor` and `limit` are ignored —
/// a count of a paginated slice would be meaningless.
pub(super) async fn count_events(
    auth: UserAuth,
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::EventsRead).await?;

    let (frag, binds) = filter_from_query(&query)?.build();
    let sql = adapt_sql(
        &format!("SELECT COUNT(*) FROM happyview_event_logs WHERE 1=1{frag}"),
        state.db_backend,
    );

    let mut q = crate::db::query_as::<(i64,)>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }
    let (count,) = q
        .fetch_one(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to count events: {e}")))?;

    Ok(Json(serde_json::json!({ "count": count })))
}

/// POST /admin/events/purge — enqueue a filtered bulk delete.
///
/// Returns `202` with a job id rather than deleting inline, matching
/// `DELETE /admin/records/collection`: the match set can run to millions of
/// rows, and running it as a job gives progress, pause and cancel.
pub(super) async fn purge_events(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<PurgeBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    auth.require(Permission::EventsPurge).await?;

    let query = EventsQuery {
        event_type: body.event_type,
        category: body.category,
        severity: body.severity,
        subject: body.subject,
        before: body.before,
        after: body.after,
        cursor: None,
        limit: None,
    };
    let filter = filter_from_query(&query)?;

    let input = serde_json::json!({
        "event_type": filter.event_type,
        "category": filter.category,
        "severity": filter.severity,
        "subject": filter.subject,
        "after": filter.after,
        "before": filter.before,
    });

    let job_id = crate::jobs::db::create_job(
        &state,
        "happyview.purge-event-logs",
        &input,
        &auth.did,
        false,
        None,
        None,
    )
    .await?;

    // Ungated by `verbose_event_logging`: a purge is a rare administrative
    // action, not per-record telemetry, and is exactly what should survive in
    // the log.
    log_event(
        &state.db,
        EventLog {
            event_type: "event_logs.purged".to_string(),
            severity: Severity::Warn,
            actor_did: Some(auth.did.clone()),
            subject: Some(job_id.clone()),
            detail: input.clone(),
        },
        state.db_backend,
    )
    .await;

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "job_id": job_id })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> EventsQuery {
        EventsQuery {
            event_type: None,
            category: None,
            severity: None,
            subject: None,
            before: None,
            after: None,
            cursor: None,
            limit: None,
        }
    }

    #[test]
    fn filter_from_query_carries_every_field() {
        let q = EventsQuery {
            event_type: Some("record.skipped".into()),
            category: Some("record".into()),
            severity: Some("info".into()),
            subject: Some("did:plc:abc".into()),
            before: Some("2026-08-01T00:00:00Z".into()),
            after: Some("2026-07-01T00:00:00Z".into()),
            ..query()
        };
        let f = filter_from_query(&q).expect("valid query");
        assert_eq!(f.event_type.as_deref(), Some("record.skipped"));
        assert_eq!(f.category.as_deref(), Some("record"));
        assert_eq!(f.severity.as_deref(), Some("info"));
        assert_eq!(f.subject.as_deref(), Some("did:plc:abc"));
    }

    #[test]
    fn filter_from_query_normalises_bounds() {
        let q = EventsQuery {
            before: Some("2026-08-01T00:00:00Z".into()),
            after: Some("2026-07-01T00:00:00Z".into()),
            ..query()
        };
        let f = filter_from_query(&q).expect("valid query");
        assert_eq!(f.before.as_deref(), Some("2026-08-01T00:00:00+00:00"));
        assert_eq!(f.after.as_deref(), Some("2026-07-01T00:00:00+00:00"));
    }

    #[test]
    fn filter_from_query_rejects_a_bad_timestamp() {
        let q = EventsQuery {
            before: Some("yesterday".into()),
            ..query()
        };
        assert!(filter_from_query(&q).is_err());
    }

    #[test]
    fn an_empty_query_filters_nothing() {
        let f = filter_from_query(&query()).expect("valid query");
        assert!(f.is_empty());
    }

    /// C1 regression guard: an empty-string text field must normalise to
    /// `None`, not `Some("")`. `Some("")` builds `AND event_type = ?` bound to
    /// `""`, so `count_events` reports 0 while the purge job (which does strip
    /// empty strings) deletes the whole table — the two endpoints disagreeing
    /// about what an empty string means.
    #[test]
    fn a_blank_text_field_is_treated_as_unset() {
        let q = EventsQuery {
            event_type: Some("".into()),
            category: Some("".into()),
            severity: Some("".into()),
            subject: Some("".into()),
            ..query()
        };
        let f = filter_from_query(&q).expect("valid query");
        assert_eq!(f.event_type, None);
        assert_eq!(f.category, None);
        assert_eq!(f.severity, None);
        assert_eq!(f.subject, None);
        assert!(f.is_empty());
    }

    #[test]
    fn a_whitespace_only_text_field_is_treated_as_unset() {
        let q = EventsQuery {
            event_type: Some("   ".into()),
            ..query()
        };
        let f = filter_from_query(&q).expect("valid query");
        assert_eq!(f.event_type, None);
    }

    #[test]
    fn a_blank_timestamp_bound_is_unset_rather_than_an_error() {
        let q = EventsQuery {
            before: Some("".into()),
            after: Some("   ".into()),
            ..query()
        };
        let f = filter_from_query(&q).expect("blank bounds must not 400");
        assert_eq!(f.before, None);
        assert_eq!(f.after, None);
    }

    #[test]
    fn a_genuine_text_value_still_passes_through_and_is_trimmed() {
        let q = EventsQuery {
            event_type: Some("  record.skipped  ".into()),
            ..query()
        };
        let f = filter_from_query(&q).expect("valid query");
        assert_eq!(f.event_type.as_deref(), Some("record.skipped"));
    }
}
