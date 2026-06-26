use std::collections::HashSet;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::auth::middleware::Claims;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};

use super::permissions::Permission;

pub struct UserAuth {
    pub did: String,
    pub user_id: String,
    pub is_super: bool,
    pub permissions: HashSet<Permission>,
    pub db: sqlx::AnyPool,
    pub db_backend: DatabaseBackend,
}

impl UserAuth {
    pub async fn require(&self, permission: Permission) -> Result<(), AppError> {
        if self.is_super || self.permissions.contains(&permission) {
            Ok(())
        } else {
            log_event(
                &self.db,
                EventLog {
                    event_type: "auth.permission_denied".to_string(),
                    severity: Severity::Warn,
                    actor_did: Some(self.did.clone()),
                    subject: Some(permission.as_str().to_string()),
                    detail: serde_json::json!({
                        "user_id": self.user_id,
                        "required_permission": permission.as_str(),
                    }),
                },
                self.db_backend,
            )
            .await;

            Err(AppError::InsufficientPermissions(
                permission.as_str().to_string(),
            ))
        }
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
        let rows: Vec<(String,)> = sqlx::query_as(&sql)
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

impl FromRequestParts<AppState> for UserAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(auth) = Self::try_api_key_auth(parts, state).await? {
            return Ok(auth);
        }

        let claims = Claims::from_request_parts(parts, state).await?;
        let did = claims.did().to_string();
        let backend = state.db_backend;

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM happyview_users")
            .fetch_one(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("user count query failed: {e}")))?;

        if count.0 == 0 {
            // Bootstrap first user
            let id = uuid::Uuid::new_v4().to_string();
            let now = now_rfc3339();

            let insert_sql = adapt_sql(
                "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, ?, ?)",
                backend,
            );

            let result = sqlx::query(&insert_sql)
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
                    let _ = sqlx::query(&perm_sql)
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
        let found: Option<(String, i32)> = sqlx::query_as(&select_sql)
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
            let _ = sqlx::query(&update_sql)
                .bind(&now)
                .bind(&uid)
                .execute(&db)
                .await;
        });

        Ok(UserAuth {
            did,
            user_id,
            is_super,
            permissions,
            db: state.db.clone(),
            db_backend: backend,
        })
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
        let backend = state.db_backend;

        let select_sql = adapt_sql(
            "SELECT k.id, u.id, u.did, u.is_super, k.permissions FROM happyview_api_keys k JOIN happyview_users u ON u.id = k.user_id WHERE k.key_hash = ? AND k.revoked_at IS NULL",
            backend,
        );

        let row: Option<(String, String, String, i32, String)> = sqlx::query_as(&select_sql)
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

        let permissions = if is_super {
            HashSet::new()
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
            let _ = sqlx::query(&update_sql)
                .bind(&now)
                .bind(&key_id)
                .execute(&db)
                .await;
        });

        Ok(Some(UserAuth {
            did,
            user_id,
            is_super,
            permissions,
            db: state.db.clone(),
            db_backend: backend,
        }))
    }
}
