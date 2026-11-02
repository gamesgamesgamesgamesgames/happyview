use std::sync::Arc;

use atrium_oauth::{KnownScope, Scope};

use crate::AppState;
use crate::auth::client_registry::build_instance_client;
use crate::error::AppError;
use crate::oauth::client_keys::{self, ClientKey, INSTANCE_OWNER};

fn domain_scopes() -> Vec<Scope> {
    vec![Scope::Known(KnownScope::Atproto)]
}

/// What the instance client advertises. The attached account signs in through
/// it and publishes the instance's lexicons with the schema scope; the plain
/// dashboard login asks for less.
pub fn instance_scopes() -> Vec<Scope> {
    vec![
        Scope::Known(KnownScope::Atproto),
        Scope::Unknown("identity:*".to_string()),
        Scope::Unknown(crate::lexicon_publishing::SCHEMA_WRITE_SCOPE.to_string()),
    ]
}

/// Switch the instance's key now and rebuild the OAuth clients around it.
/// Authorization servers that cached the old key set refuse the new key
/// until their cache runs out; [`stage_instance_key`] avoids that.
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

    install_instance_key(state, &new_key).await;
    Ok((new_key, orphaned_sessions))
}

/// Publish a new instance key that takes over after
/// [`client_keys::ROTATION_OVERLAP`]. Returns it and when it takes over;
/// [`run_key_promotion`] switches the clients over then. With no current
/// key to keep signing, the new one is installed at once and the time is
/// `None`.
pub async fn stage_instance_key(state: &AppState) -> Result<(ClientKey, Option<String>), AppError> {
    let (key, activates_at) = client_keys::stage_key(
        &state.db,
        state.db_backend,
        state.config.token_encryption_key.as_ref(),
        INSTANCE_OWNER,
    )
    .await?;
    if activates_at.is_none() {
        install_instance_key(state, &key).await;
    }
    Ok((key, activates_at))
}

/// The longest [`run_key_promotion`] sleeps. A key staged while it sleeps
/// is promoted at most this long after it comes due.
const PROMOTION_POLL: std::time::Duration = std::time::Duration::from_secs(60);

/// Promote the staged keys as they come due, and rebuild the instance's
/// clients when its key has changed underneath them, whether this process
/// promoted it or another did. Wakes when the next staged key is due, and
/// at least every [`PROMOTION_POLL`]. Spawned by the boot.
pub async fn run_key_promotion(state: AppState) {
    loop {
        if let Err(e) = promote_and_install(&state).await {
            tracing::warn!(error = %e, "failed to promote staged OAuth client keys");
        }
        let wait = match client_keys::next_activation(&state.db, state.db_backend).await {
            Ok(Some(at)) => (at - chrono::Utc::now())
                .to_std()
                .unwrap_or_default()
                .min(PROMOTION_POLL),
            _ => PROMOTION_POLL,
        };
        tokio::time::sleep(wait).await;
    }
}

/// One pass of [`run_key_promotion`].
pub async fn promote_and_install(state: &AppState) -> Result<(), AppError> {
    let promoted = client_keys::promote_due_keys(&state.db, state.db_backend).await?;
    for owner in &promoted {
        tracing::info!(owner, "a staged OAuth client key took over");
    }
    let current = client_keys::load_keys(
        &state.db,
        state.db_backend,
        state.config.token_encryption_key.as_ref(),
        INSTANCE_OWNER,
    )
    .await?
    .into_iter()
    .find(|k| k.status == client_keys::KeyStatus::Current);
    let Some(current) = current else {
        return Ok(());
    };
    // A loopback instance's primary client never signs, so it records no
    // kid to compare; only a promotion here tells it the key changed.
    let installed = state.oauth.primary_client_and_kid().1;
    let changed = match installed {
        Some(kid) => kid != current.kid,
        None => promoted.iter().any(|o| o == INSTANCE_OWNER),
    };
    if changed {
        install_instance_key(state, &current).await;
    }
    Ok(())
}

/// Rebuild the instance's OAuth clients (primary, every domain, linked
/// repos) to sign with `new_key`.
async fn install_instance_key(state: &AppState, new_key: &ClientKey) {
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
        new_key,
        state.config.token_encryption_key.as_ref(),
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
            new_key,
            state.config.token_encryption_key.as_ref(),
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

    match crate::oauth::client_keys::to_atrium_jwk(new_key) {
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
            state.config.token_encryption_key.as_ref(),
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
}
