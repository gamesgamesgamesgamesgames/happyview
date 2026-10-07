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
use crate::event_log::{EventLog, Severity, log_event};
use crate::feature_flags::{self, FeatureFlag};

use super::auth::UserAuth;
use super::permissions::Permission;

pub const MAX_GRANT_SETTING: &str = "space_inspector_max_grant_minutes";
pub const DEFAULT_GRANT_MINUTES: i64 = 60;
pub const MIN_GRANT_MINUTES: i64 = 5;
pub const MAX_REASON_CHARS: usize = 2000;

pub struct InspectorConfig {
    pub enabled: bool,
    pub max_grant_minutes: i64,
}

/// The instance's maximum grant length. Unparseable values fall back to the
/// default; anything below the minimum is raised to it.
pub fn parse_max_minutes(raw: Option<&str>) -> i64 {
    raw.and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_GRANT_MINUTES)
        .max(MIN_GRANT_MINUTES)
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
    auth.require(Permission::SpacesRead).await?;
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

async fn insert_grant(pool: &AnyPool, backend: DatabaseBackend, g: &Grant) -> Result<(), AppError> {
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
        .execute(pool)
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

/// A user's own unrevoked grants, with expiry checked in Rust rather than in
/// SQL. Unlike `list_user_grants`, this has no row limit: the authorization
/// path must see every grant the caller holds, not just the most recent 200.
async fn unrevoked_user_grants(
    pool: &AnyPool,
    backend: DatabaseBackend,
    user_id: &str,
) -> Result<Vec<Grant>, AppError> {
    let sql = adapt_sql(
        &format!(
            "SELECT {GRANT_COLUMNS} FROM happyview_space_access_grants WHERE user_id = ? AND revoked_at IS NULL"
        ),
        backend,
    );
    let rows: Vec<GrantRow> = crate::db::query_as(&sql)
        .bind(user_id)
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
    #[allow(dead_code)] // No account-wide read route constructs this yet.
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

/// The caller's active grants, after checking the permission and the switch.
pub async fn active_grants(state: &AppState, auth: &UserAuth) -> Result<Vec<Grant>, AppError> {
    auth.require(Permission::SpacesInspect).await?;
    if !load_config(&state.db, state.db_backend).await.enabled {
        return Err(inspector_disabled());
    }
    let now = chrono::Utc::now();
    Ok(
        unrevoked_user_grants(&state.db, state.db_backend, &auth.user_id)
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

async fn mark_revoked(
    pool: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
    revoked_by: &str,
    at: &str,
) -> Result<(), AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_space_access_grants SET revoked_at = ?, revoked_by = ? WHERE id = ? AND revoked_at IS NULL",
        backend,
    );
    crate::db::query(&sql)
        .bind(at)
        .bind(revoked_by)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke access grant: {e}")))?;
    Ok(())
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
    insert_grant(&state.db, state.db_backend, &grant).await?;

    log_event(
        &state.db,
        EventLog {
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
        },
        state.db_backend,
    )
    .await;

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
    let now = chrono::Utc::now();
    let grants: Vec<Grant> = list_user_grants(&state.db, state.db_backend, &auth.user_id)
        .await?
        .into_iter()
        .filter(|g| !params.active.unwrap_or(false) || g.is_active(now))
        .collect();
    Ok(Json(serde_json::json!({ "grants": grants })))
}

/// DELETE /admin/spaces/access-grants/{id} — end a grant early. Owners end
/// their own; `users:update` ends anyone's.
pub(super) async fn revoke_grant(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<Grant>, AppError> {
    let grant = get_grant(&state.db, state.db_backend, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("Access grant not found".into()))?;
    if grant.user_id == auth.user_id {
        auth.require(Permission::SpacesInspect).await?;
    } else {
        auth.require(Permission::UsersUpdate).await?;
    }
    if !grant.is_active(chrono::Utc::now()) {
        return Ok(Json(grant));
    }

    let at = now_rfc3339();
    mark_revoked(&state.db, state.db_backend, &grant.id, &auth.user_id, &at).await?;
    log_event(
        &state.db,
        EventLog {
            event_type: "space.access_revoked".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(grant.target.clone()),
            detail: serde_json::json!({
                "grant_id": grant.id,
                "revoked_by": auth.did,
                "user_id": grant.user_id,
            }),
        },
        state.db_backend,
    )
    .await;

    Ok(Json(Grant {
        revoked_at: Some(at),
        revoked_by: Some(auth.user_id.clone()),
        ..grant
    }))
}

/// GET /admin/spaces/access-grants/{id}/reads — reads made under a grant,
/// oldest first.
pub(super) async fn grant_reads(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::EventsRead).await?;
    // `detail` is compact JSON text on both backends, so the grant id appears
    // as `"grant_id":"<id>"`.
    let pattern = format!("%\"grant_id\":\"{}\"%", crate::db::escape_like(&id));
    let sql = adapt_sql(
        "SELECT id, event_type, severity, actor_did, subject, detail, created_at FROM happyview_event_logs WHERE event_type = 'space.moderator_read' AND detail LIKE ? ESCAPE '\\' ORDER BY created_at ASC LIMIT 500",
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
    )> = crate::db::query_as(&sql)
        .bind(&pattern)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load grant reads: {e}")))?;
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
    Ok(Json(serde_json::json!({ "events": events })))
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
