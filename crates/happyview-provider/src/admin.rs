use axum::extract::FromRequestParts;
use axum::http::{StatusCode, request::Parts};
use axum::response::{IntoResponse, Response};

/// The authenticated admin making a request, inserted by the host's auth layer.
#[derive(Clone, Debug)]
pub struct AdminCaller {
    pub did: String,
    /// `Permission::as_str()` values from HappyView's admin permissions.
    pub permissions: Vec<String>,
    /// The request came with the managed-hosting platform key, not from a
    /// user. HappyView's own routes refuse it unless they opt in: take
    /// [`AdminUser`] to refuse it as they do.
    pub platform: bool,
}

impl AdminCaller {
    pub fn has(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }

    /// `Ok` when the caller holds `permission`, otherwise the refusal
    /// HappyView's own routes give, which the host also logs.
    pub fn require(&self, permission: &str) -> Result<(), AdminRefusal> {
        if self.has(permission) {
            Ok(())
        } else {
            Err(AdminRefusal::MissingPermission(permission.to_string()))
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AdminCaller {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AdminCaller>()
            .cloned()
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

/// An [`AdminCaller`] that is not the platform principal. Extracting it
/// refuses the platform key as HappyView's own routes do.
#[derive(Clone, Debug)]
pub struct AdminUser(pub AdminCaller);

impl<S: Send + Sync> FromRequestParts<S> for AdminUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let caller = AdminCaller::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        if caller.platform {
            return Err(AdminRefusal::Platform.into_response());
        }
        Ok(Self(caller))
    }
}

/// Why an admin route refused its caller, answered as HappyView answers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminRefusal {
    /// `403 {"error": "InsufficientPermissions", ...}`, marked with
    /// [`PermissionDenied`] so the host logs the refusal.
    MissingPermission(String),
    /// `403 {"error": "the platform key cannot use this endpoint"}`.
    Platform,
}

impl IntoResponse for AdminRefusal {
    fn into_response(self) -> Response {
        match self {
            AdminRefusal::MissingPermission(permission) => {
                let body = serde_json::json!({
                    "error": "InsufficientPermissions",
                    "message": format!("Missing permission: {permission}"),
                });
                let mut response = (StatusCode::FORBIDDEN, axum::Json(body)).into_response();
                response
                    .extensions_mut()
                    .insert(PermissionDenied(permission));
                response
            }
            AdminRefusal::Platform => (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({
                    "error": "the platform key cannot use this endpoint"
                })),
            )
                .into_response(),
        }
    }
}

/// On a response: the route refused its caller for want of this permission.
/// The host's auth layer logs it as it logs its own routes' refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionDenied(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(platform: bool) -> AdminCaller {
        AdminCaller {
            did: "did:plc:a".into(),
            permissions: vec!["records:read".into(), "backfill:create".into()],
            platform,
        }
    }

    #[test]
    fn has_checks_the_permission_list() {
        let caller = caller(false);
        assert!(caller.has("backfill:create"));
        assert!(!caller.has("records:delete"));
    }

    #[test]
    fn a_missing_permission_is_refused_and_marked() {
        assert_eq!(caller(false).require("backfill:create"), Ok(()));
        let refusal = caller(false).require("records:delete").unwrap_err();
        let response = refusal.into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.extensions().get::<PermissionDenied>(),
            Some(&PermissionDenied("records:delete".into()))
        );
    }

    #[tokio::test]
    async fn an_admin_user_is_never_the_platform_principal() {
        let (mut parts, ()) = axum::http::Request::new(()).into_parts();
        parts.extensions.insert(caller(true));
        let refused = AdminUser::from_request_parts(&mut parts, &())
            .await
            .unwrap_err();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);

        let (mut parts, ()) = axum::http::Request::new(()).into_parts();
        parts.extensions.insert(caller(false));
        assert!(AdminUser::from_request_parts(&mut parts, &()).await.is_ok());
    }
}
