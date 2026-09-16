use serde::Serialize;

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_NEEDS_REAUTH: &str = "needs_reauth";

#[derive(Debug, Clone, Serialize)]
pub struct LinkedRepo {
    pub id: String,
    pub did: Option<String>,
    pub handle: Option<String>,
    pub reason: Option<String>,
    pub scopes: String,
    pub status: String,
    pub last_error: Option<String>,
    pub last_refreshed_at: Option<String>,
    pub authorized_at: Option<String>,
    pub created_by: String,
    pub created_at: String,
}
