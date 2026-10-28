//! hpy, the HappyView CLI, as an API client every instance knows.
//!
//! hpy signs in as a public native atproto OAuth client whose metadata is a
//! static document hosted off-instance, and registers the session through
//! `/oauth/sessions` like any API client, so no instance needs setting up
//! before a user can sign in with it. Reaching the admin API takes the same
//! grant from hpy as from any app; see `auth::admin_scope`.

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;

/// The row's id, fixed so a re-seed finds it.
pub const CLIENT_ID: &str = "builtin-hpy";

/// Not a secret: a public client's key never is. Published in `/config`.
pub const CLIENT_KEY: &str = "hvc_hpy";

pub const CLIENT_ID_URL: &str = "https://happyproto.at/oauth/hpy-cli/client-metadata.json";

const CLIENT_URI: &str = "https://happyproto.at";

const NAME: &str = "hpy";

/// Loopback redirects; RFC 8252 lets the port vary.
const REDIRECT_URIS: &[&str] = &[
    "http://127.0.0.1/oauth/callback",
    "http://[::1]/oauth/callback",
];

/// What the client may ask for. hpy writes nothing to the PDS; the include is
/// what lets it use the admin API.
const SCOPES: &str = "atproto include:dev.happyview.adminFull";

/// Writes the built-in row, restoring any field that has drifted. Runs at
/// every boot, so new and existing instances alike carry it.
pub async fn seed(db: &sqlx::AnyPool, backend: DatabaseBackend) -> Result<(), AppError> {
    let now = now_rfc3339();
    let redirect_uris = serde_json::to_string(REDIRECT_URIS).expect("static strings serialise");
    let sql = adapt_sql(
        "INSERT INTO happyview_api_clients (id, client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, scopes, client_type, allowed_origins, is_active, created_by, created_at, updated_at) \
         VALUES (?, ?, '', ?, ?, ?, ?, ?, 'public', NULL, 1, 'happyview', ?, ?) \
         ON CONFLICT (id) DO UPDATE SET client_key = excluded.client_key, client_secret_hash = '', name = excluded.name, \
         client_id_url = excluded.client_id_url, client_uri = excluded.client_uri, redirect_uris = excluded.redirect_uris, \
         scopes = excluded.scopes, client_type = 'public', allowed_origins = NULL, is_active = 1",
        backend,
    );
    crate::db::query(&sql)
        .bind(CLIENT_ID)
        .bind(CLIENT_KEY)
        .bind(NAME)
        .bind(CLIENT_ID_URL)
        .bind(CLIENT_URI)
        .bind(&redirect_uris)
        .bind(SCOPES)
        .bind(&now)
        .bind(&now)
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to seed the hpy API client: {e}")))?;
    Ok(())
}

/// The admin API's refusal for an attempt to change the built-in row, which
/// the next boot would restore anyway.
pub fn refuse_change(id: &str) -> Result<(), AppError> {
    if id == CLIENT_ID {
        return Err(AppError::Conflict(
            "the hpy client is built in and cannot be changed or removed".into(),
        ));
    }
    Ok(())
}
