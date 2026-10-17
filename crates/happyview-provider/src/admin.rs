use axum::extract::FromRequestParts;
use axum::http::{StatusCode, request::Parts};

/// The authenticated admin making a request, inserted by the host's auth layer.
#[derive(Clone, Debug)]
pub struct AdminCaller {
    pub did: String,
    /// `Permission::as_str()` values from HappyView's admin permissions.
    pub permissions: Vec<String>,
}

impl AdminCaller {
    pub fn has(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_checks_the_permission_list() {
        let caller = AdminCaller {
            did: "did:plc:a".into(),
            permissions: vec!["records:read".into(), "backfill:create".into()],
        };
        assert!(caller.has("backfill:create"));
        assert!(!caller.has("records:delete"));
    }
}
