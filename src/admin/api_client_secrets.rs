//! `/admin/api-clients/{id}/secrets`: a confidential client's secrets,
//! listed, added and revoked one at a time.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::db::adapt_sql;
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::oauth::client_secrets::{self, ClientSecret, Revoked};
use crate::rate_limit::SecretHash;

use super::auth::UserAuth;
use super::permissions::Permission;

struct Client {
    name: String,
    client_key: String,
    client_type: String,
}

async fn client(state: &AppState, id: &str) -> Result<Client, AppError> {
    let sql = adapt_sql(
        "SELECT name, client_key, client_type FROM happyview_api_clients WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String, String, String)> = crate::db::query_as(&sql)
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get api client: {e}")))?;
    let (name, client_key, client_type) =
        row.ok_or_else(|| AppError::NotFound(format!("api client '{id}' not found")))?;
    Ok(Client {
        name,
        client_key,
        client_type,
    })
}

#[derive(Serialize)]
pub(super) struct SecretList {
    secrets: Vec<ClientSecret>,
}

/// GET /admin/api-clients/{id}/secrets — every secret the client has held,
/// revoked ones included. Never the secrets themselves.
pub(super) async fn list(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<SecretList>, AppError> {
    auth.require(Permission::ApiClientsView).await?;
    client(&state, &id).await?;
    let secrets = client_secrets::list(&state.db, state.db_backend, &id).await?;
    Ok(Json(SecretList { secrets }))
}

#[derive(Deserialize)]
pub(super) struct CreateSecretBody {
    label: String,
}

#[derive(Serialize)]
pub(super) struct CreatedSecret {
    #[serde(flatten)]
    secret: ClientSecret,
    /// The plaintext. This response is the only place it ever appears.
    client_secret: String,
}

/// POST /admin/api-clients/{id}/secrets — add a secret alongside the
/// client's others. It works at once; the others keep working.
pub(super) async fn create(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    Json(body): Json<CreateSecretBody>,
) -> Result<(StatusCode, Json<CreatedSecret>), AppError> {
    auth.require(Permission::ApiClientsEdit).await?;
    crate::auth::cli_client::refuse_change(&id)?;
    let label = client_secrets::checked_label(&body.label)?;
    let client = client(&state, &id).await?;
    if client.client_type != "confidential" {
        return Err(AppError::BadRequest(
            "only a confidential client has secrets; a public client is identified by its key and origin".into(),
        ));
    }

    let (secret, plaintext) =
        client_secrets::create(&state.db, state.db_backend, &id, &label).await?;
    // An inactive client has no identity registered; it is built from the
    // database, this secret included, when the client is reactivated.
    state.rate_limiter.add_client_secret(
        &client.client_key,
        SecretHash {
            id: secret.id.clone(),
            hash: client_secrets::hash(&plaintext),
        },
    );

    log_event(
        &state.db,
        EventLog {
            event_type: "api_client.secret_created".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(client.name),
            detail: serde_json::json!({
                "client_id": id,
                "secret_id": secret.id,
                "label": secret.label,
            }),
        },
        state.db_backend,
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(CreatedSecret {
            secret,
            client_secret: plaintext,
        }),
    ))
}

#[derive(Serialize)]
pub(super) struct RevokedSecret {
    /// False when the secret had been revoked already.
    revoked: bool,
    /// How many of the client's secrets still work. Zero means the client
    /// can no longer authenticate with a secret until one is added.
    active_secrets: usize,
}

/// DELETE /admin/api-clients/{id}/secrets/{secret_id} — revoke one secret.
/// It stops working immediately; the client's other secrets are untouched.
pub(super) async fn revoke(
    State(state): State<AppState>,
    auth: UserAuth,
    Path((id, secret_id)): Path<(String, String)>,
) -> Result<Json<RevokedSecret>, AppError> {
    auth.require(Permission::ApiClientsEdit).await?;
    crate::auth::cli_client::refuse_change(&id)?;
    let client = client(&state, &id).await?;

    let revoked = match client_secrets::revoke(&state.db, state.db_backend, &id, &secret_id).await?
    {
        Revoked::Now => true,
        Revoked::Already => false,
        Revoked::NotFound => {
            return Err(AppError::NotFound(format!(
                "api client '{id}' has no secret '{secret_id}'"
            )));
        }
    };
    state
        .rate_limiter
        .revoke_client_secret(&client.client_key, &secret_id);
    let active_secrets = client_secrets::active_hashes(&state.db, state.db_backend, &id)
        .await?
        .len();

    if revoked {
        log_event(
            &state.db,
            EventLog {
                event_type: "api_client.secret_revoked".to_string(),
                severity: Severity::Info,
                actor_did: Some(auth.did.clone()),
                subject: Some(client.name),
                detail: serde_json::json!({
                    "client_id": id,
                    "secret_id": secret_id,
                    "active_secrets": active_secrets,
                }),
            },
            state.db_backend,
        )
        .await;
    }

    Ok(Json(RevokedSecret {
        revoked,
        active_secrets,
    }))
}
