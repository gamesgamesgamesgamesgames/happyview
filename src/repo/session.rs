use atrium_api::types::string::Did;

use crate::AppState;
use crate::HappyViewOAuthSession;
use crate::db::adapt_sql;
use crate::error::AppError;

/// Resolve an API client ID from a client_key.
/// Used by the procedure handler to route DPoP PDS writes.
pub(crate) async fn get_dpop_client_id(
    state: &AppState,
    client_key: &str,
) -> Result<String, AppError> {
    let sql = adapt_sql(
        "SELECT id FROM happyview_api_clients WHERE client_key = ? AND is_active = 1",
        state.db_backend,
    );

    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind(client_key)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up API client: {e}")))?;

    row.map(|(id,)| id)
        .ok_or_else(|| AppError::Auth("unknown API client".into()))
}

/// Resume an OAuth session for the given DID via atrium.
/// The returned `OAuthSession` handles DPoP and token refresh internally.
pub(crate) async fn get_oauth_session(
    state: &AppState,
    did: &str,
) -> Result<HappyViewOAuthSession, AppError> {
    let did =
        Did::new(did.to_string()).map_err(|_| AppError::Auth(format!("invalid DID: {did}")))?;
    state
        .oauth
        .primary_client()
        .restore(&did)
        .await
        .map_err(|e| AppError::Auth(format!("no OAuth session for {}: {e}", did.as_ref())))
}
