use std::collections::HashSet;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::auth::middleware::Claims;
use crate::constant_time::ct_eq_str;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};

use super::permissions::Permission;

/// The `did` recorded in event logs for actions taken by the managed-hosting
/// platform principal. It is not a DID and never resolves; it marks the actor.
pub const PLATFORM_ACTOR: &str = "platform";

/// What the platform principal may do, beyond its dedicated checks
/// (`require_domain_access`, `is_platform`). Deliberately tiny.
const PLATFORM_PERMISSIONS: &[Permission] = &[Permission::StatsRead];

#[derive(Clone)]
pub struct UserAuth {
    pub did: String,
    pub user_id: String,
    pub is_super: bool,
    /// The request was authenticated with the managed-hosting platform key
    /// (`PLATFORM_API_KEY_HASH`). Never true for a user or a user's API key.
    pub is_platform: bool,
    pub permissions: HashSet<Permission>,
    pub db: sqlx::AnyPool,
    pub db_backend: DatabaseBackend,
}

impl UserAuth {
    pub fn has(&self, permission: Permission) -> bool {
        self.is_super || self.permissions.contains(&permission)
    }

    /// Domain management: the platform principal, or anyone holding
    /// `settings:manage`. Kept separate from `require` so the platform principal
    /// can manage domains without receiving `settings:manage`, which covers far
    /// more than domains.
    pub async fn require_domain_access(&self) -> Result<(), AppError> {
        if self.is_platform {
            return Ok(());
        }
        self.require(Permission::SettingsManage).await
    }

    pub async fn require(&self, permission: Permission) -> Result<(), AppError> {
        if self.has(permission) {
            Ok(())
        } else {
            self.log_permission_denied(permission.as_str()).await;

            Err(AppError::InsufficientPermissions(
                permission.as_str().to_string(),
            ))
        }
    }

    /// The `auth.permission_denied` event for a refusal for want of
    /// `permission`.
    pub(crate) async fn log_permission_denied(&self, permission: &str) {
        log_event(
            &self.db,
            EventLog {
                event_type: "auth.permission_denied".to_string(),
                severity: Severity::Warn,
                actor_did: Some(self.did.clone()),
                subject: Some(permission.to_string()),
                detail: serde_json::json!({
                    "user_id": self.user_id,
                    "required_permission": permission,
                }),
            },
            self.db_backend,
        )
        .await;
    }

    async fn load_permissions(
        db: &sqlx::AnyPool,
        user_id: &str,
        backend: DatabaseBackend,
    ) -> Result<HashSet<Permission>, AppError> {
        let sql = adapt_sql(
            "SELECT permission FROM happyview_user_permissions WHERE user_id = ?",
            backend,
        );
        let rows: Vec<(String,)> = crate::db::query_as(&sql)
            .bind(user_id)
            .fetch_all(db)
            .await
            .map_err(|e| AppError::Internal(format!("permission query failed: {e}")))?;

        let mut perms = HashSet::new();
        for (perm_str,) in rows {
            if let Ok(p) = serde_json::from_value::<Permission>(serde_json::Value::String(perm_str))
            {
                perms.insert(p);
            }
        }
        Ok(perms)
    }

    async fn load_api_key_permissions(
        db: &sqlx::AnyPool,
        user_id: &str,
        key_permissions: &[String],
        backend: DatabaseBackend,
    ) -> Result<HashSet<Permission>, AppError> {
        let user_perms = Self::load_permissions(db, user_id, backend).await?;
        let mut effective = HashSet::new();
        for perm_str in key_permissions {
            #[allow(clippy::collapsible_if)]
            if let Ok(p) =
                serde_json::from_value::<Permission>(serde_json::Value::String(perm_str.clone()))
            {
                if user_perms.contains(&p) {
                    effective.insert(p);
                }
            }
        }
        Ok(effective)
    }
}

impl UserAuth {
    /// Authenticate the request, platform principal included. Callers decide
    /// whether the platform principal is acceptable; see `UserAuth`'s extractor
    /// and `AllowPlatform`.
    async fn authenticate(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        if let Some(auth) = Self::try_api_key_auth(parts, state).await? {
            return Ok(auth);
        }

        let claims = Claims::from_request_parts(parts, state).await?;
        let did = claims.did().to_string();
        let backend = state.db_backend;

        let count: (i64,) = crate::db::query_as("SELECT COUNT(*) FROM happyview_users")
            .fetch_one(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("user count query failed: {e}")))?;

        // A configured superadmin is seeded at boot. If that seeding failed the
        // table is still empty, and promoting whoever signs in first would hand
        // the instance to a stranger instead of the account the file names.
        if count.0 == 0 && state.config.superadmin_did.is_none() {
            // Bootstrap first user
            let id = uuid::Uuid::new_v4().to_string();
            let now = now_rfc3339();

            let insert_sql = adapt_sql(
                "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, ?, ?)",
                backend,
            );

            let result = crate::db::query(&insert_sql)
                .bind(&id)
                .bind(&did)
                .bind(1_i32)
                .bind(&now)
                .execute(&state.db)
                .await;

            if result.is_ok() {
                let perm_sql = adapt_sql(
                    "INSERT INTO happyview_user_permissions (user_id, permission, granted_at) VALUES (?, ?, ?)",
                    backend,
                );
                for perm in Permission::all() {
                    let _ = crate::db::query(&perm_sql)
                        .bind(&id)
                        .bind(perm.as_str())
                        .bind(&now)
                        .execute(&state.db)
                        .await;
                }

                tracing::info!(did = %did, "auto-bootstrapped first super user");

                log_event(
                    &state.db,
                    EventLog {
                        event_type: "user.bootstrapped".to_string(),
                        severity: Severity::Info,
                        actor_did: None,
                        subject: Some(did.clone()),
                        detail: serde_json::json!({}),
                    },
                    backend,
                )
                .await;
            }
        }

        let select_sql = adapt_sql(
            "SELECT id, is_super FROM happyview_users WHERE did = ?",
            backend,
        );
        let found: Option<(String, i32)> = crate::db::query_as(&select_sql)
            .bind(&did)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("user auth query failed: {e}")))?;

        let Some((user_id, is_super_int)) = found else {
            return Err(AppError::Forbidden("not a user".into()));
        };
        let is_super = is_super_int != 0;

        let permissions = if is_super {
            HashSet::new()
        } else {
            Self::load_permissions(&state.db, &user_id, backend).await?
        };

        let db = state.db.clone();
        let uid = user_id.clone();
        let now = now_rfc3339();
        let update_sql = adapt_sql(
            "UPDATE happyview_users SET last_used_at = ? WHERE id = ?",
            backend,
        );
        tokio::spawn(async move {
            let _ = crate::db::query(&update_sql)
                .bind(&now)
                .bind(&uid)
                .execute(&db)
                .await;
        });

        Ok(UserAuth {
            did,
            user_id,
            is_super,
            is_platform: false,
            permissions,
            db: state.db.clone(),
            db_backend: backend,
        })
    }
}

/// Deny by default: every handler that takes `UserAuth` refuses the managed-
/// hosting platform principal, so a new route can never be reachable with the
/// platform key by accident. Routes it may call opt in with `AllowPlatform`.
impl FromRequestParts<AppState> for UserAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = Self::authenticate(parts, state).await?;
        if auth.is_platform {
            return Err(AppError::Forbidden(
                "the platform key cannot use this endpoint".into(),
            ));
        }
        Ok(auth)
    }
}

/// In front of a record source's admin routes: authenticate the request as
/// `AllowPlatform` does, refuse it as a handler would when that fails, and
/// hand the source an [`AdminCaller`](happyview_provider::AdminCaller). The
/// source checks the caller's permissions itself (and refuses the platform
/// principal through `AdminUser`); a refusal it marks with
/// [`PermissionDenied`](happyview_provider::PermissionDenied) is logged here
/// as `UserAuth::require` logs one.
pub(crate) async fn require_admin_caller(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let (mut parts, body) = request.into_parts();
    let auth = match UserAuth::authenticate(&mut parts, &state).await {
        Ok(auth) => auth,
        Err(e) => return e.into_response(),
    };
    let permissions = if auth.is_super {
        Permission::all()
    } else {
        auth.permissions.clone()
    };
    let mut permissions: Vec<String> = permissions.iter().map(|p| p.as_str().to_string()).collect();
    permissions.sort();
    parts.extensions.insert(happyview_provider::AdminCaller {
        did: auth.did.clone(),
        permissions,
        platform: auth.is_platform,
    });
    let response = next
        .run(axum::extract::Request::from_parts(parts, body))
        .await;
    // A source refused the caller for a permission: log it as `require`
    // logs a refusal by HappyView's own routes.
    if let Some(happyview_provider::PermissionDenied(permission)) =
        response
            .extensions()
            .get::<happyview_provider::PermissionDenied>()
    {
        auth.log_permission_denied(permission).await;
    }
    response
}

/// Explicit opt-in for routes the managed-hosting platform may call. Behaves
/// like `UserAuth` but also accepts the platform principal; the handler must
/// still check what that principal is allowed to do.
pub struct AllowPlatform(pub UserAuth);

impl FromRequestParts<AppState> for AllowPlatform {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        UserAuth::authenticate(parts, state).await.map(Self)
    }
}

impl UserAuth {
    async fn try_api_key_auth(parts: &Parts, state: &AppState) -> Result<Option<Self>, AppError> {
        let header = match parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
        {
            Some(h) => h,
            None => return Ok(None),
        };

        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("DPoP "));

        let token = match token {
            Some(t) if t.starts_with("hv_") => t,
            _ => return Ok(None),
        };

        let hash = hex::encode(Sha256::digest(token.as_bytes()));

        if let Some(platform_hash) = state.config.platform_api_key_hash.as_deref()
            && ct_eq_str(&hash, platform_hash)
        {
            return Ok(Some(UserAuth {
                did: PLATFORM_ACTOR.to_string(),
                user_id: String::new(),
                is_super: false,
                is_platform: true,
                permissions: PLATFORM_PERMISSIONS.iter().copied().collect(),
                db: state.db.clone(),
                db_backend: state.db_backend,
            }));
        }
        let backend = state.db_backend;

        let select_sql = adapt_sql(
            "SELECT k.id, u.id, u.did, u.is_super, k.permissions FROM happyview_api_keys k JOIN happyview_users u ON u.id = k.user_id WHERE k.key_hash = ? AND k.revoked_at IS NULL",
            backend,
        );

        let row: Option<(String, String, String, i32, String)> = crate::db::query_as(&select_sql)
            .bind(&hash)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("api key lookup failed: {e}")))?;

        let Some((key_id, user_id, did, is_super_int, permissions_json)) = row else {
            return Err(AppError::Auth("invalid or revoked API key".into()));
        };
        let is_super = is_super_int != 0;

        // Parse permissions from JSON string (stored as JSON array)
        let key_permissions: Vec<String> =
            serde_json::from_str(&permissions_json).unwrap_or_default();

        // An API key is bounded by its own stored permission list and never
        // carries super privileges — otherwise a super admin's "read-only" key
        // would grant full admin (H5). A super user implicitly holds every
        // permission, so their key's list is used as-is; a non-super user's key
        // is additionally intersected with what that user actually holds.
        let permissions = if is_super {
            parse_permissions(&key_permissions)
        } else {
            Self::load_api_key_permissions(&state.db, &user_id, &key_permissions, backend).await?
        };

        let db = state.db.clone();
        let now = now_rfc3339();
        let update_sql = adapt_sql(
            "UPDATE happyview_api_keys SET last_used_at = ? WHERE id = ?",
            backend,
        );
        tokio::spawn(async move {
            let _ = crate::db::query(&update_sql)
                .bind(&now)
                .bind(&key_id)
                .execute(&db)
                .await;
        });

        Ok(Some(UserAuth {
            did,
            user_id,
            // A key is never super, regardless of its owner: `require()` must
            // consult the key's permissions, and super-only operations (user
            // management, transfer_super) stay unavailable via API keys.
            is_super: false,
            is_platform: false,
            permissions,
            db: state.db.clone(),
            db_backend: backend,
        }))
    }
}

/// Parse a stored key permission list (JSON strings) into a permission set,
/// dropping any unrecognized entries. Used for super-user keys, whose owner
/// implicitly holds every permission so no intersection is needed.
fn parse_permissions(key_permissions: &[String]) -> HashSet<Permission> {
    key_permissions
        .iter()
        .filter_map(|s| {
            serde_json::from_value::<Permission>(serde_json::Value::String(s.clone())).ok()
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuperadminSeed {
    Created,
    Promoted,
    Unchanged,
}

/// Make the configured account a super user. Additive only: other super
/// users keep their role, because the configuration names who must be an
/// admin, not who must not be.
pub(crate) async fn ensure_configured_superadmin(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    did: &str,
) -> Result<SuperadminSeed, sqlx::Error> {
    let now = now_rfc3339();
    let mut tx = db.begin().await?;

    let existing: Option<(String, i32)> = crate::db::query_as(&adapt_sql(
        "SELECT id, is_super FROM happyview_users WHERE did = ?",
        backend,
    ))
    .bind(did)
    .fetch_optional(&mut *tx)
    .await?;

    let (user_id, seed) = match existing {
        Some((_, is_super)) if is_super != 0 => return Ok(SuperadminSeed::Unchanged),
        Some((id, _)) => {
            crate::db::query(&adapt_sql(
                "UPDATE happyview_users SET is_super = ? WHERE id = ?",
                backend,
            ))
            .bind(1_i32)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
            (id, SuperadminSeed::Promoted)
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
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            (id, SuperadminSeed::Created)
        }
    };

    let perm_sql = adapt_sql(
        "INSERT INTO happyview_user_permissions (user_id, permission, granted_at) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
        backend,
    );
    for perm in Permission::all() {
        crate::db::query(&perm_sql)
            .bind(&user_id)
            .bind(perm.as_str())
            .bind(&now)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    log_event(
        db,
        EventLog {
            event_type: "user.bootstrapped".to_string(),
            severity: Severity::Info,
            actor_did: None,
            subject: Some(did.to_string()),
            detail: serde_json::json!({ "source": "config" }),
        },
        backend,
    )
    .await;

    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::migrated_memory_pool;

    async fn user(db: &sqlx::AnyPool, did: &str) -> Option<(String, i32)> {
        crate::db::query_as("SELECT id, is_super FROM happyview_users WHERE did = ?")
            .bind(did)
            .fetch_optional(db)
            .await
            .unwrap()
    }

    async fn insert_user(db: &sqlx::AnyPool, did: &str, is_super: i32) {
        crate::db::query(
            "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(did)
        .bind(is_super)
        .bind(now_rfc3339())
        .execute(db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn an_absent_superadmin_is_created_with_every_permission() {
        let db = migrated_memory_pool().await;
        let seed = ensure_configured_superadmin(&db, DatabaseBackend::Sqlite, "did:plc:boss")
            .await
            .unwrap();
        assert_eq!(seed, SuperadminSeed::Created);
        let (id, is_super) = user(&db, "did:plc:boss").await.unwrap();
        assert_eq!(is_super, 1);
        let (granted,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM happyview_user_permissions WHERE user_id = ?",
        )
        .bind(&id)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(granted as usize, Permission::all().len());
    }

    #[tokio::test]
    async fn an_existing_user_is_promoted_and_others_keep_their_role() {
        let db = migrated_memory_pool().await;
        insert_user(&db, "did:plc:boss", 0).await;
        insert_user(&db, "did:plc:other-super", 1).await;
        let seed = ensure_configured_superadmin(&db, DatabaseBackend::Sqlite, "did:plc:boss")
            .await
            .unwrap();
        assert_eq!(seed, SuperadminSeed::Promoted);
        assert_eq!(user(&db, "did:plc:boss").await.unwrap().1, 1);
        assert_eq!(user(&db, "did:plc:other-super").await.unwrap().1, 1);
    }

    #[tokio::test]
    async fn seeding_twice_creates_one_row() {
        let db = migrated_memory_pool().await;
        let first = ensure_configured_superadmin(&db, DatabaseBackend::Sqlite, "did:plc:boss")
            .await
            .unwrap();
        let second = ensure_configured_superadmin(&db, DatabaseBackend::Sqlite, "did:plc:boss")
            .await
            .unwrap();
        assert_eq!(first, SuperadminSeed::Created);
        assert_eq!(second, SuperadminSeed::Unchanged);
        let (rows,): (i64,) =
            crate::db::query_as("SELECT COUNT(*) FROM happyview_users WHERE did = ?")
                .bind("did:plc:boss")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(rows, 1);
    }

    /// Run `UserAuth::authenticate` for `did` over a signed session cookie,
    /// against an empty users table.
    async fn sign_in_to_empty_instance(
        configured: Option<&str>,
        did: &str,
    ) -> (Result<UserAuth, AppError>, sqlx::AnyPool) {
        use axum::response::IntoResponse;
        use axum_extra::extract::cookie::{Cookie, SignedCookieJar};

        let db = migrated_memory_pool().await;
        let mut state =
            crate::test_support::test_state_with_pool_on(db.clone(), DatabaseBackend::Sqlite);
        state.config.session_secret = "a-securely-generated-32plus-byte-secret!!".into();
        state.config.superadmin_did = configured.map(str::to_string);

        let jar = SignedCookieJar::new(state.cookie_key.clone())
            .add(Cookie::new(crate::auth::COOKIE_NAME, did.to_string()));
        let set_cookie = jar.into_response();
        let cookie = set_cookie.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let (mut parts, _) = axum::http::Request::builder()
            .header("cookie", cookie)
            .body(())
            .unwrap()
            .into_parts();

        (UserAuth::authenticate(&mut parts, &state).await, db)
    }

    #[tokio::test]
    async fn a_configured_superadmin_stops_the_first_sign_in_becoming_admin() {
        let (auth, db) = sign_in_to_empty_instance(Some("did:plc:boss"), "did:plc:stranger").await;
        assert!(auth.is_err());
        assert!(user(&db, "did:plc:stranger").await.is_none());
    }

    #[tokio::test]
    async fn without_a_configured_superadmin_the_first_sign_in_becomes_admin() {
        let (auth, db) = sign_in_to_empty_instance(None, "did:plc:first").await;
        assert!(auth.is_ok());
        assert_eq!(user(&db, "did:plc:first").await.unwrap().1, 1);
    }

    #[tokio::test]
    async fn a_superadmin_already_in_place_is_left_alone() {
        let db = migrated_memory_pool().await;
        insert_user(&db, "did:plc:boss", 1).await;
        let seed = ensure_configured_superadmin(&db, DatabaseBackend::Sqlite, "did:plc:boss")
            .await
            .unwrap();
        assert_eq!(seed, SuperadminSeed::Unchanged);
    }
}
