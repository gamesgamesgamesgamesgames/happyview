pub mod create_client;
pub mod delete_client;
pub mod get_client;
pub mod list_clients;

pub use create_client::create_api_client;
pub use delete_client::delete_api_client;
pub use get_client::get_api_client;
pub use list_clients::list_api_clients;

use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::Claims;
use crate::db::adapt_sql;
use crate::error::AppError;

/// Whose API clients a request may read or delete.
pub(crate) enum Manager {
    /// The user at this instance, with no client key: all the clients they own.
    Owner,
    /// A top-level app: only the clients it created, by its `id`. Holding a
    /// user's session does not let one app reach the clients the user made
    /// through another.
    App(String),
}

impl Manager {
    pub(crate) async fn of(state: &AppState, claims: &Claims) -> Result<Self, AppError> {
        let Some(client_key) = claims.client_key() else {
            return Ok(Self::Owner);
        };
        let sql = adapt_sql(
            "SELECT id, parent_client_id FROM happyview_api_clients WHERE client_key = ?",
            state.db_backend,
        );
        let row: Option<(String, Option<String>)> = crate::db::query_as(&sql)
            .bind(client_key)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("client lookup failed: {e}")))?;
        match row {
            Some((id, None)) => Ok(Self::App(id)),
            Some((_, Some(_))) => Err(AppError::Forbidden(
                "Child clients cannot manage API clients".into(),
            )),
            None => Err(AppError::Auth("Invalid client".into())),
        }
    }

    /// The condition to add to a query already filtered by `owner_did`, and
    /// the app id it binds, if any.
    pub(crate) fn filter(self) -> (&'static str, Option<String>) {
        match self {
            Self::Owner => ("", None),
            Self::App(id) => (" AND parent_client_id = ?", Some(id)),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiClientView {
    pub id: String,
    pub name: String,
    pub client_key: String,
    pub client_id_url: String,
    pub client_uri: String,
    pub redirect_uris: Vec<String>,
    pub client_type: String,
    pub scopes: String,
    pub allowed_origins: Vec<String>,
    pub is_active: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiClientViewWithSecret {
    #[serde(flatten)]
    pub client: ApiClientView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiClientInput {
    pub name: String,
    pub client_id_url: String,
    pub client_uri: String,
    pub redirect_uris: Vec<String>,
    #[serde(default = "default_client_type")]
    pub client_type: String,
    #[serde(default = "default_scopes")]
    pub scopes: String,
    pub allowed_origins: Option<Vec<String>>,
}

fn default_client_type() -> String {
    "confidential".to_string()
}

fn default_scopes() -> String {
    "atproto".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteApiClientInput {
    pub id: String,
}

/// Build an ApiClientView from a database row.
///
/// Note: `is_active` is stored as i32 in the database (SQLite/Postgres compat via AnyPool).
/// The existing codebase uses `row.get()` with the `sqlx::Row` trait. We follow the same
/// pattern here but use `try_get` to return a Result rather than panic on missing columns.
pub fn row_to_view(row: &sqlx::any::AnyRow) -> Result<ApiClientView, sqlx::Error> {
    use sqlx::Row;

    let redirect_uris_raw: String = row.try_get("redirect_uris")?;
    let redirect_uris: Vec<String> = serde_json::from_str(&redirect_uris_raw).unwrap_or_default();

    let allowed_origins_raw: Option<String> = row.try_get("allowed_origins")?;
    let allowed_origins: Vec<String> = allowed_origins_raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    // is_active is stored as i32 (0/1) for SQLite/Postgres compatibility,
    // matching the pattern used throughout admin/api_clients.rs.
    let is_active_raw: i32 = row.try_get("is_active")?;

    Ok(ApiClientView {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        client_key: row.try_get("client_key")?,
        client_id_url: row.try_get("client_id_url")?,
        client_uri: row.try_get("client_uri")?,
        redirect_uris,
        client_type: row.try_get("client_type")?,
        scopes: row.try_get("scopes")?,
        allowed_origins,
        is_active: is_active_raw != 0,
        created_at: row.try_get("created_at")?,
    })
}
