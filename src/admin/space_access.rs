//! Moderator access to space contents.
//!
//! Reading a private space is opt-in per instance, and each read happens under
//! a grant: a moderator names one space or one account, says why, and gets
//! access for a limited time. Grants and the reads made under them are
//! protected events (see `event_log::PROTECTED_EVENT_TYPES`).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::AnyPool;

use crate::AppState;
use crate::admin::settings::get_setting;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339, parse_dt};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, write_event};
use crate::feature_flags::{self, FeatureFlag};

use super::auth::UserAuth;
use super::permissions::Permission;

pub const MAX_GRANT_SETTING: &str = "space_inspector_max_grant_minutes";
pub const DEFAULT_GRANT_MINUTES: i64 = 60;
pub const MIN_GRANT_MINUTES: i64 = 5;
/// The longest grant the instance can be configured for: one year. Keeps the
/// expiry inside what date arithmetic can represent.
pub const MAX_GRANT_MINUTES: i64 = 365 * 24 * 60;
pub const MAX_REASON_CHARS: usize = 2000;

pub struct InspectorConfig {
    pub enabled: bool,
    pub max_grant_minutes: i64,
}

/// The instance's maximum grant length. Unparseable values fall back to the
/// default, and the result is kept between the minimum and one year.
pub fn parse_max_minutes(raw: Option<&str>) -> i64 {
    raw.and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_GRANT_MINUTES)
        .clamp(MIN_GRANT_MINUTES, MAX_GRANT_MINUTES)
}

pub async fn load_config(pool: &AnyPool, backend: DatabaseBackend) -> InspectorConfig {
    InspectorConfig {
        enabled: feature_flags::is_enabled(pool, FeatureFlag::SPACE_INSPECTOR, backend).await,
        max_grant_minutes: parse_max_minutes(
            get_setting(pool, MAX_GRANT_SETTING, backend)
                .await
                .as_deref(),
        ),
    }
}

/// GET /admin/spaces/inspector — whether moderators can request access, and
/// for how long.
pub(super) async fn inspector_status(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    // Either permission: a moderator who can request access needs to know
    // whether they can, even without `spaces:read`.
    if !auth.has(Permission::SpacesInspect) {
        auth.require(Permission::SpacesRead).await?;
    }
    let config = load_config(&state.db, state.db_backend).await;
    Ok(Json(serde_json::json!({
        "enabled": config.enabled,
        "default_grant_minutes": DEFAULT_GRANT_MINUTES.min(config.max_grant_minutes),
        "max_grant_minutes": config.max_grant_minutes,
    })))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantScope {
    Space,
    Account,
}

impl GrantScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Space => "space",
            Self::Account => "account",
        }
    }

    fn parse(s: &str) -> Result<Self, AppError> {
        match s {
            "space" => Ok(Self::Space),
            "account" => Ok(Self::Account),
            other => Err(AppError::Internal(format!("unknown grant scope '{other}'"))),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Grant {
    pub id: String,
    pub user_id: String,
    pub user_did: String,
    pub scope: GrantScope,
    pub target: String,
    pub reason: String,
    pub created_at: String,
    pub expires_at: String,
    pub revoked_at: Option<String>,
    pub revoked_by: Option<String>,
}

impl Grant {
    pub fn is_active(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.revoked_at.is_none() && parse_dt(&self.expires_at) > now
    }
}

pub fn clamp_duration(requested: Option<i64>, max: i64) -> i64 {
    requested
        .unwrap_or(DEFAULT_GRANT_MINUTES)
        .clamp(MIN_GRANT_MINUTES, max.max(MIN_GRANT_MINUTES))
}

pub fn validate_reason(raw: &str) -> Result<String, AppError> {
    let reason = raw.trim();
    if reason.is_empty() {
        return Err(AppError::BadRequest("A reason is required".into()));
    }
    if reason.chars().count() > MAX_REASON_CHARS {
        return Err(AppError::BadRequest(format!(
            "Reason must be at most {MAX_REASON_CHARS} characters"
        )));
    }
    Ok(reason.to_string())
}

pub fn inspector_disabled() -> AppError {
    AppError::XrpcError {
        status: StatusCode::FORBIDDEN,
        code: "SpaceInspectorDisabled",
        message: "Space inspector is disabled on this instance".into(),
    }
}

pub fn grant_required() -> AppError {
    AppError::XrpcError {
        status: StatusCode::FORBIDDEN,
        code: "SpaceAccessGrantRequired",
        message: "Request access to this space or account before reading its contents".into(),
    }
}

type GrantRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

const GRANT_COLUMNS: &str =
    "id, user_id, user_did, scope, target, reason, created_at, expires_at, revoked_at, revoked_by";

fn parse_grant(r: GrantRow) -> Result<Grant, AppError> {
    Ok(Grant {
        id: r.0,
        user_id: r.1,
        user_did: r.2,
        scope: GrantScope::parse(&r.3)?,
        target: r.4,
        reason: r.5,
        created_at: r.6,
        expires_at: r.7,
        revoked_at: r.8,
        revoked_by: r.9,
    })
}

async fn insert_grant<'e, E>(
    executor: E,
    backend: DatabaseBackend,
    g: &Grant,
) -> Result<(), AppError>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let sql = adapt_sql(
        &format!(
            "INSERT INTO happyview_space_access_grants ({GRANT_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ),
        backend,
    );
    crate::db::query(&sql)
        .bind(&g.id)
        .bind(&g.user_id)
        .bind(&g.user_did)
        .bind(g.scope.as_str())
        .bind(&g.target)
        .bind(&g.reason)
        .bind(&g.created_at)
        .bind(&g.expires_at)
        .bind(&g.revoked_at)
        .bind(&g.revoked_by)
        .execute(executor)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create access grant: {e}")))?;
    Ok(())
}

async fn get_grant(
    pool: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<Option<Grant>, AppError> {
    let sql = adapt_sql(
        &format!("SELECT {GRANT_COLUMNS} FROM happyview_space_access_grants WHERE id = ?"),
        backend,
    );
    let row: Option<GrantRow> = crate::db::query_as(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load access grant: {e}")))?;
    row.map(parse_grant).transpose()
}

/// A user's grants, newest first. Expiry is checked in Rust with `parse_dt`
/// rather than in SQL, where TEXT timestamps compare as strings.
pub async fn list_user_grants(
    pool: &AnyPool,
    backend: DatabaseBackend,
    user_id: &str,
) -> Result<Vec<Grant>, AppError> {
    let sql = adapt_sql(
        &format!(
            "SELECT {GRANT_COLUMNS} FROM happyview_space_access_grants WHERE user_id = ? ORDER BY created_at DESC LIMIT 200"
        ),
        backend,
    );
    let rows: Vec<GrantRow> = crate::db::query_as(&sql)
        .bind(user_id)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list access grants: {e}")))?;
    rows.into_iter().map(parse_grant).collect()
}

/// A user's own unrevoked, unexpired grants. Unlike `list_user_grants`, this
/// has no row limit: the authorization path must see every grant the caller
/// holds, not just the most recent 200.
///
/// Grants store `expires_at` as UTC `to_rfc3339()` text, which sorts as a
/// string, so the SQL bound skips long-expired rows. Callers still check
/// `Grant::is_active`, which is the authority on expiry.
async fn unexpired_user_grants(
    pool: &AnyPool,
    backend: DatabaseBackend,
    user_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<Grant>, AppError> {
    let sql = adapt_sql(
        &format!(
            "SELECT {GRANT_COLUMNS} FROM happyview_space_access_grants WHERE user_id = ? AND revoked_at IS NULL AND expires_at > ?"
        ),
        backend,
    );
    let rows: Vec<GrantRow> = crate::db::query_as(&sql)
        .bind(user_id)
        .bind(now.to_rfc3339())
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list active access grants: {e}")))?;
    rows.into_iter().map(parse_grant).collect()
}

/// What a content read must be covered by.
pub enum Covers<'a> {
    /// Records in one space. With `repo`, an account grant for that author
    /// also covers the request.
    Space {
        space_id: &'a str,
        repo: Option<&'a str>,
    },
    /// One account's records in any space.
    Account { did: &'a str },
}

fn covers(grant: &Grant, request: &Covers) -> bool {
    match (grant.scope, request) {
        (GrantScope::Space, Covers::Space { space_id, .. }) => grant.target == *space_id,
        (
            GrantScope::Account,
            Covers::Space {
                repo: Some(repo), ..
            },
        ) => grant.target == *repo,
        (GrantScope::Account, Covers::Account { did }) => grant.target == *did,
        _ => false,
    }
}

/// The active grant covering a request, preferring the one that expires last.
pub fn select_covering<'g>(
    grants: &'g [Grant],
    request: &Covers,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<&'g Grant> {
    grants
        .iter()
        .filter(|g| g.is_active(now) && covers(g, request))
        .max_by_key(|g| parse_dt(&g.expires_at))
}

/// Fails with `SpaceInspectorDisabled` unless the instance switch is on. Every
/// space moderation route checks it, metadata included, so turning the
/// inspector off closes the whole area.
pub async fn require_enabled(state: &AppState) -> Result<(), AppError> {
    if load_config(&state.db, state.db_backend).await.enabled {
        Ok(())
    } else {
        Err(inspector_disabled())
    }
}

/// The caller's active grants, after checking the permission and the switch.
pub async fn active_grants(state: &AppState, auth: &UserAuth) -> Result<Vec<Grant>, AppError> {
    auth.require(Permission::SpacesInspect).await?;
    require_enabled(state).await?;
    let now = chrono::Utc::now();
    Ok(
        unexpired_user_grants(&state.db, state.db_backend, &auth.user_id, now)
            .await?
            .into_iter()
            .filter(|g| g.is_active(now))
            .collect(),
    )
}

/// The grant covering a request, or `SpaceAccessGrantRequired` if none does.
pub async fn require_grant(
    state: &AppState,
    auth: &UserAuth,
    request: Covers<'_>,
) -> Result<Grant, AppError> {
    let grants = active_grants(state, auth).await?;
    select_covering(&grants, &request, chrono::Utc::now())
        .cloned()
        .ok_or_else(grant_required)
}

async fn mark_revoked<'e, E>(
    executor: E,
    backend: DatabaseBackend,
    id: &str,
    revoked_by: &str,
    at: &str,
) -> Result<bool, AppError>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let sql = adapt_sql(
        "UPDATE happyview_space_access_grants SET revoked_at = ?, revoked_by = ? WHERE id = ? AND revoked_at IS NULL",
        backend,
    );
    let result = crate::db::query(&sql)
        .bind(at)
        .bind(revoked_by)
        .bind(id)
        .execute(executor)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke access grant: {e}")))?;
    // Zero rows means another request revoked it first.
    Ok(result.rows_affected() > 0)
}

async fn begin(state: &AppState) -> Result<sqlx::Transaction<'static, sqlx::Any>, AppError> {
    state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))
}

/// Tie a grant or revocation event to its grant, so retention keeps it while
/// the grant row exists.
async fn link_grant_event(
    tx: &mut sqlx::Transaction<'static, sqlx::Any>,
    backend: DatabaseBackend,
    event_id: &str,
    grant_id: &str,
) -> Result<(), AppError> {
    crate::db::query(&adapt_sql(
        "INSERT INTO happyview_space_access_grant_events (event_id, grant_id) VALUES (?, ?)",
        backend,
    ))
    .bind(event_id)
    .bind(grant_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| AppError::Internal(format!("failed to link the grant event: {e}")))?;
    Ok(())
}

async fn commit(tx: sqlx::Transaction<'static, sqlx::Any>) -> Result<(), AppError> {
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit transaction: {e}")))
}

fn grant_subject(grant: &Grant, space: Option<&crate::spaces::types::Space>) -> String {
    match (grant.scope, space) {
        (GrantScope::Space, Some(s)) => format!("at://{}/space/{}/{}", s.did, s.type_nsid, s.skey),
        _ => grant.target.clone(),
    }
}

#[derive(Deserialize)]
pub(super) struct CreateGrantBody {
    pub scope: GrantScope,
    pub target: String,
    pub reason: String,
    pub duration_minutes: Option<i64>,
}

/// POST /admin/spaces/access-grants
pub(super) async fn create_grant(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<CreateGrantBody>,
) -> Result<(StatusCode, Json<Grant>), AppError> {
    auth.require(Permission::SpacesInspect).await?;
    let config = load_config(&state.db, state.db_backend).await;
    if !config.enabled {
        return Err(inspector_disabled());
    }
    let reason = validate_reason(&body.reason)?;
    let target = body.target.trim().to_string();

    let space = match body.scope {
        GrantScope::Space => Some(
            crate::spaces::db::get_space(&state.db, state.db_backend, &target)
                .await?
                .ok_or_else(|| AppError::NotFound("Space not found".into()))?,
        ),
        GrantScope::Account => {
            atrium_api::types::string::Did::new(target.clone())
                .map_err(|_| AppError::BadRequest(format!("'{target}' is not a valid DID")))?;
            None
        }
    };

    let minutes = clamp_duration(body.duration_minutes, config.max_grant_minutes);
    let now = chrono::Utc::now();
    let grant = Grant {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: auth.user_id.clone(),
        user_did: auth.did.clone(),
        scope: body.scope,
        target,
        reason,
        created_at: now.to_rfc3339(),
        expires_at: (now + chrono::Duration::minutes(minutes)).to_rfc3339(),
        revoked_at: None,
        revoked_by: None,
    };
    let event = EventLog {
        event_type: "space.access_granted".to_string(),
        severity: Severity::Info,
        actor_did: Some(auth.did.clone()),
        subject: Some(grant_subject(&grant, space.as_ref())),
        detail: serde_json::json!({
            "grant_id": grant.id,
            "scope": grant.scope.as_str(),
            "target": grant.target,
            "reason": grant.reason,
            "expires_at": grant.expires_at,
            "user_id": grant.user_id,
        }),
    };

    // The grant and the event recording its reason commit together: a grant
    // with no audit record must not exist.
    let mut tx = begin(&state).await?;
    insert_grant(&mut *tx, state.db_backend, &grant).await?;
    let event_id = write_event(&mut *tx, &event, state.db_backend)
        .await
        .map_err(|e| AppError::Internal(format!("failed to record the access grant: {e}")))?;
    link_grant_event(&mut tx, state.db_backend, &event_id, &grant.id).await?;
    commit(tx).await?;

    Ok((StatusCode::CREATED, Json(grant)))
}

#[derive(Deserialize)]
pub(super) struct ListGrantsParams {
    pub active: Option<bool>,
}

/// GET /admin/spaces/access-grants — the caller's own grants.
pub(super) async fn list_grants(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(params): Query<ListGrantsParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SpacesInspect).await?;
    let grants: Vec<Grant> = if params.active.unwrap_or(false) {
        // Active grants come from the uncapped query, so an older long-lived
        // grant is never pushed out of the list by newer ones.
        let now = chrono::Utc::now();
        let mut active: Vec<Grant> =
            unexpired_user_grants(&state.db, state.db_backend, &auth.user_id, now)
                .await?
                .into_iter()
                .filter(|g| g.is_active(now))
                .collect();
        active.sort_by_key(|g| std::cmp::Reverse(parse_dt(&g.created_at)));
        active
    } else {
        list_user_grants(&state.db, state.db_backend, &auth.user_id).await?
    };
    Ok(Json(serde_json::json!({ "grants": grants })))
}

/// DELETE /admin/spaces/access-grants/{id} — end a grant early. Owners end
/// their own with `spaces:inspect` or `users:update`; `users:update` ends
/// anyone's.
pub(super) async fn revoke_grant(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<Grant>, AppError> {
    let grant = get_grant(&state.db, state.db_backend, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("Access grant not found".into()))?;
    if grant.user_id != auth.user_id {
        auth.require(Permission::UsersUpdate).await?;
    } else if !auth.has(Permission::UsersUpdate) {
        auth.require(Permission::SpacesInspect).await?;
    }
    if !grant.is_active(chrono::Utc::now()) {
        return Ok(Json(grant));
    }

    // The space may have been deleted since the grant was made; the subject
    // then falls back to the raw target.
    let space = match grant.scope {
        GrantScope::Space => {
            crate::spaces::db::get_space(&state.db, state.db_backend, &grant.target)
                .await
                .ok()
                .flatten()
        }
        GrantScope::Account => None,
    };
    let event = EventLog {
        event_type: "space.access_revoked".to_string(),
        severity: Severity::Info,
        actor_did: Some(auth.did.clone()),
        subject: Some(grant_subject(&grant, space.as_ref())),
        detail: serde_json::json!({
            "grant_id": grant.id,
            "revoked_by": auth.did,
            "user_id": grant.user_id,
        }),
    };

    let at = now_rfc3339();
    let mut tx = begin(&state).await?;
    if !mark_revoked(&mut *tx, state.db_backend, &grant.id, &auth.user_id, &at).await? {
        // A concurrent request revoked it and wrote the event; return its
        // result rather than record a second revocation.
        drop(tx);
        let current = get_grant(&state.db, state.db_backend, &grant.id)
            .await?
            .ok_or_else(|| AppError::NotFound("Access grant not found".into()))?;
        return Ok(Json(current));
    }
    let event_id = write_event(&mut *tx, &event, state.db_backend)
        .await
        .map_err(|e| AppError::Internal(format!("failed to record the revocation: {e}")))?;
    link_grant_event(&mut tx, state.db_backend, &event_id, &grant.id).await?;
    commit(tx).await?;

    Ok(Json(Grant {
        revoked_at: Some(at),
        revoked_by: Some(auth.user_id.clone()),
        ..grant
    }))
}

#[derive(Deserialize)]
pub(super) struct GrantReadsParams {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

/// GET /admin/spaces/access-grants/{id}/reads — reads made under a grant,
/// oldest first, a page at a time. `cursor` is present while more remain.
pub(super) async fn grant_reads(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    Query(params): Query<GrantReadsParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::EventsRead).await?;
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let after = params.cursor.as_deref().and_then(crate::db::decode_cursor);

    let after_clause = if after.is_some() {
        " AND (e.created_at > ? OR (e.created_at = ? AND e.id > ?))"
    } else {
        ""
    };
    let sql = adapt_sql(
        &format!(
            "SELECT e.id, e.event_type, e.severity, e.actor_did, e.subject, e.detail, e.created_at FROM happyview_space_access_reads r JOIN happyview_event_logs e ON e.id = r.event_id WHERE r.grant_id = ?{after_clause} ORDER BY e.created_at ASC, e.id ASC LIMIT ?"
        ),
        state.db_backend,
    );
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
    )> = {
        let mut q = crate::db::query_as(&sql).bind(&id);
        if let Some((ts, last_id)) = &after {
            q = q.bind(ts).bind(ts).bind(last_id);
        }
        q.bind(limit)
            .fetch_all(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to load grant reads: {e}")))?
    };
    let cursor = (rows.len() as i64 == limit)
        .then(|| rows.last().map(|r| crate::db::encode_cursor(&r.6, &r.0)))
        .flatten();
    let events: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, event_type, severity, actor_did, subject, detail, created_at)| {
            serde_json::json!({
                "id": id,
                "event_type": event_type,
                "severity": severity,
                "actor_did": actor_did,
                "subject": subject,
                "detail": serde_json::from_str::<serde_json::Value>(&detail).unwrap_or_default(),
                "created_at": parse_dt(&created_at),
            })
        })
        .collect();
    let mut body = serde_json::json!({ "events": events });
    if let Some(cursor) = cursor {
        body["cursor"] = cursor.into();
    }
    Ok(Json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_minutes_parsing() {
        assert_eq!(parse_max_minutes(None), 60);
        assert_eq!(parse_max_minutes(Some("abc")), 60);
        assert_eq!(parse_max_minutes(Some("0")), 5);
        assert_eq!(parse_max_minutes(Some("-10")), 5);
        assert_eq!(parse_max_minutes(Some(" 90 ")), 90);
        assert_eq!(parse_max_minutes(Some("100000")), 100000);
        assert_eq!(
            parse_max_minutes(Some("9223372036854775807")),
            MAX_GRANT_MINUTES
        );
        assert_eq!(
            clamp_duration(
                Some(i64::MAX),
                parse_max_minutes(Some("9223372036854775807"))
            ),
            MAX_GRANT_MINUTES,
            "a huge setting can't overflow the expiry"
        );
    }

    #[test]
    fn clamp_handles_bad_max_values() {
        assert_eq!(clamp_duration(None, parse_max_minutes(Some("abc"))), 60);
        assert_eq!(clamp_duration(None, parse_max_minutes(Some("0"))), 5);
        assert_eq!(clamp_duration(Some(30), parse_max_minutes(Some("-10"))), 5);
        assert_eq!(
            clamp_duration(Some(100000), parse_max_minutes(Some("100000"))),
            100000
        );
        assert_eq!(clamp_duration(Some(0), 60), 5);
        assert_eq!(clamp_duration(Some(-3), 60), 5);
        assert_eq!(clamp_duration(Some(90), 60), 60);
        assert_eq!(
            clamp_duration(None, 30),
            30,
            "default never exceeds the max"
        );
    }

    #[test]
    fn reasons_are_trimmed_and_bounded() {
        assert!(validate_reason("   \n\t ").is_err());
        assert_eq!(validate_reason("  report #4  ").unwrap(), "report #4");
        assert!(
            validate_reason(&"🙂".repeat(2000)).is_ok(),
            "the cap counts characters, not bytes"
        );
        assert!(validate_reason(&"a".repeat(2001)).is_err());
    }

    fn grant(scope: GrantScope, target: &str, minutes: i64) -> Grant {
        let now = chrono::Utc::now();
        Grant {
            id: format!("{target}-{minutes}"),
            user_id: "u".into(),
            user_did: "did:plc:u".into(),
            scope,
            target: target.into(),
            reason: "r".into(),
            created_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::minutes(minutes)).to_rfc3339(),
            revoked_at: None,
            revoked_by: None,
        }
    }

    #[test]
    fn coverage_rules() {
        let now = chrono::Utc::now();
        let grants = vec![
            grant(GrantScope::Space, "s1", 10),
            grant(GrantScope::Account, "did:plc:a", 30),
            grant(GrantScope::Space, "s2", -1),
        ];
        let space_all = Covers::Space {
            space_id: "s1",
            repo: None,
        };
        assert_eq!(
            select_covering(&grants, &space_all, now).unwrap().target,
            "s1"
        );
        let other_space = Covers::Space {
            space_id: "s3",
            repo: None,
        };
        assert!(select_covering(&grants, &other_space, now).is_none());
        let expired = Covers::Space {
            space_id: "s2",
            repo: None,
        };
        assert!(select_covering(&grants, &expired, now).is_none());
        let author_elsewhere = Covers::Space {
            space_id: "s3",
            repo: Some("did:plc:a"),
        };
        assert_eq!(
            select_covering(&grants, &author_elsewhere, now)
                .unwrap()
                .target,
            "did:plc:a"
        );
        let both = Covers::Space {
            space_id: "s1",
            repo: Some("did:plc:a"),
        };
        assert_eq!(
            select_covering(&grants, &both, now).unwrap().target,
            "did:plc:a",
            "the grant expiring latest wins"
        );
        let account = Covers::Account { did: "did:plc:b" };
        assert!(select_covering(&grants, &account, now).is_none());
    }
}
