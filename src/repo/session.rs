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

    let row: Option<(String,)> = crate::db::query_as(&sql)
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

    let signing_kid = crate::auth::oauth_store::lookup_signing_kid(
        &state.db,
        state.db_backend,
        "happyview_oauth_sessions",
        did.as_ref(),
    )
    .await
    .map_err(|e| AppError::Internal(format!("failed to look up session signing key: {e}")))?;

    let client = match signing_kid {
        Some(kid) => {
            let client_id_url = state.config.instance_client_id_url();
            state
                .oauth
                .get_for_kid(&client_id_url, &kid)
                .ok_or_else(|| {
                    AppError::Auth(format!(
                        "the key session for {} was established with is no longer available; the user must re-authenticate",
                        did.as_ref()
                    ))
                })?
        }
        None => {
            let (client, kid) = state.oauth.primary_client_and_kid();
            if let Some(kid) = kid
                && let Err(e) = crate::auth::oauth_store::stamp_signing_kid_if_unset(
                    &state.db,
                    state.db_backend,
                    "happyview_oauth_sessions",
                    "did",
                    did.as_ref(),
                    &kid,
                )
                .await
            {
                tracing::warn!(error = %e, "failed to lazily stamp signing_kid");
            }
            client
        }
    };

    client
        .restore(&did)
        .await
        .map_err(|e| AppError::Auth(format!("no OAuth session for {}: {e}", did.as_ref())))
}
