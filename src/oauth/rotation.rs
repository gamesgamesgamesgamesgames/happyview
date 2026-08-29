use std::sync::Arc;

use atrium_oauth::{KnownScope, Scope};

use crate::AppState;
use crate::auth::client_registry::build_instance_client;
use crate::error::AppError;
use crate::oauth::client_keys::{self, ClientKey, INSTANCE_OWNER};

fn domain_scopes() -> Vec<Scope> {
    vec![Scope::Known(KnownScope::Atproto)]
}

fn instance_scopes() -> Vec<Scope> {
    vec![
        Scope::Known(KnownScope::Atproto),
        Scope::Unknown("identity:*".to_string()),
    ]
}

pub async fn rotate_instance_key(state: &AppState) -> Result<(ClientKey, u64), AppError> {
    let new_key = client_keys::rotate_key(
        &state.db,
        state.db_backend,
        state.config.token_encryption_key.as_ref(),
        INSTANCE_OWNER,
    )
    .await?;

    let orphaned_sessions =
        client_keys::count_unstamped_sessions(&state.db, state.db_backend, INSTANCE_OWNER).await?;

    let is_loopback = crate::auth::client_registry::is_loopback_url(&state.config.public_url);
    let instance_client_id_url = state.config.instance_client_id_url();
    let callback_url = format!(
        "{}/auth/callback",
        state.config.effective_public_url().trim_end_matches('/')
    );

    match build_instance_client(
        &state.config.plc_url,
        &instance_client_id_url,
        &state.config.effective_public_url(),
        vec![callback_url.clone()],
        is_loopback,
        instance_scopes(),
        state.oauth_state_store.clone(),
        state.db.clone(),
        state.db_backend,
        &new_key,
    ) {
        Ok(client) => {
            let client = Arc::new(client);
            let primary_kid = if is_loopback {
                None
            } else {
                Some(new_key.kid.clone())
            };
            state.oauth.register_for_kid(
                &instance_client_id_url,
                &new_key.kid,
                Arc::clone(&client),
            );
            state
                .oauth
                .set_primary_client_with_kid(Arc::clone(&client), primary_kid);

            if let Some(pd) = state.domain_cache.primary().await {
                let primary_client_id_url = format!(
                    "{}/oauth-client-metadata.json",
                    state
                        .config
                        .url_with_base_path(&pd.url)
                        .trim_end_matches('/')
                );
                let domain_kid = if is_loopback {
                    None
                } else {
                    Some(new_key.kid.clone())
                };
                state.oauth.register_domain_client(
                    pd.url.clone(),
                    primary_client_id_url,
                    client,
                    domain_kid,
                );
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to rebuild primary OAuth client after key rotation");
        }
    }

    for domain in state.domain_cache.all().await {
        if domain.is_primary {
            continue;
        }

        let domain_base_url = state.config.url_with_base_path(&domain.url);
        let domain_callback_url =
            format!("{}/auth/callback", domain_base_url.trim_end_matches('/'));
        let domain_client_id = format!(
            "{}/oauth-client-metadata.json",
            domain_base_url.trim_end_matches('/')
        );

        match build_instance_client(
            &state.config.plc_url,
            &domain_client_id,
            &domain_base_url,
            vec![domain_callback_url],
            false,
            domain_scopes(),
            state.oauth_state_store.clone(),
            state.db.clone(),
            state.db_backend,
            &new_key,
        ) {
            Ok(client) => {
                let client = Arc::new(client);
                state.oauth.register_domain_client(
                    domain.url.clone(),
                    domain_client_id.clone(),
                    Arc::clone(&client),
                    if is_loopback {
                        None
                    } else {
                        Some(new_key.kid.clone())
                    },
                );
                state
                    .oauth
                    .register_for_kid(&domain_client_id, &new_key.kid, client);
            }
            Err(e) => {
                tracing::error!(
                    domain = %domain.url,
                    error = %e,
                    "failed to rebuild domain OAuth client after key rotation"
                );
            }
        }
    }

    match crate::oauth::client_keys::to_atrium_jwk(&new_key) {
        Ok(jwk) => match crate::linked_repos::client::build(
            &state.config.plc_url,
            &instance_client_id_url,
            &state.config.effective_public_url(),
            callback_url,
            is_loopback,
            instance_scopes(),
            state.oauth_state_store.clone(),
            state.db.clone(),
            state.db_backend,
            Some(vec![jwk]),
        ) {
            Ok(client) => {
                let client = Arc::new(client);
                state
                    .oauth
                    .register_linked_repos_for_kid(&new_key.kid, Arc::clone(&client));
                state
                    .oauth
                    .set_linked_repos_primary(client, Some(new_key.kid.clone()));
            }
            Err(e) => tracing::error!(
                error = %e,
                "failed to build per-kid linked-repos OAuth client after rotation"
            ),
        },
        Err(e) => tracing::error!(
            error = %e,
            "failed to convert rotated key to JWK for linked-repos registration"
        ),
    }

    Ok((new_key, orphaned_sessions))
}
