use axum::response::Response;
use serde_json::Value;

use crate::AppState;
use crate::auth::Claims;
use crate::auth::ServiceAuthClaims;
use crate::error::AppError;
use crate::repo;

pub(crate) async fn handle_procedure(
    state: &AppState,
    method: &str,
    claims: &Claims,
    input: &Value,
    params: &std::collections::HashMap<String, Value>,
    lexicon: &crate::lexicon::ParsedLexicon,
    service_auth: Option<&ServiceAuthClaims>,
) -> Result<Response, AppError> {
    // A procedure is its script, bound at `xrpc.procedure:<id>`; one with no
    // script has nothing to run.
    let trigger =
        crate::lua::ParsedTrigger::new(crate::lua::TriggerKind::XrpcProcedure, &lexicon.id);
    // One lookup for the whole call: the scope check below and the run after
    // it must describe the same script, whatever the snapshot does meanwhile.
    let project = state.project.load_full();
    let resolved = state.scripts.resolve_in(&project, &trigger).await;

    // Service auth access and scope checks
    if let Some(sa) = &service_auth {
        let has_access = crate::service_entries::check_access(
            &state.db,
            state.db_backend,
            &sa.aud_fragment,
            method,
        )
        .await?;

        if !has_access {
            crate::event_log::log_event(
                &state.db,
                crate::event_log::EventLog {
                    event_type: "service_auth.access_denied".to_string(),
                    severity: crate::event_log::Severity::Error,
                    actor_did: Some(sa.did.clone()),
                    subject: Some(method.to_string()),
                    detail: serde_json::json!({
                        "fragment": sa.aud_fragment,
                        "reason": "service entry not authorized for this XRPC"
                    }),
                },
                state.db_backend,
            )
            .await;

            return Err(AppError::Auth(format!(
                "service '{}' is not authorized for '{}'",
                sa.aud_fragment, method
            )));
        }

        // Check token scope against outbound XRPCs declared by the script
        if let Some(script) = &resolved {
            let token_scope: Vec<&str> = vec![&method];
            let missing: Vec<&String> = script
                .outbound_xrpcs
                .iter()
                .filter(|x| !token_scope.contains(&x.as_str()))
                .collect();

            if !missing.is_empty() {
                let missing_list: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
                return Err(AppError::Auth(format!(
                    "this procedure calls additional XRPCs not covered by the token scope: {}",
                    missing_list.join(", ")
                )));
            }
        }
    }
    // A forged caller holds no DPoP session, so the delegation guard below
    // would refuse it for missing authentication. A test acts as an account
    // by naming it.
    if state.test.is_some() && input.get("delegateDid").is_some() {
        return Err(AppError::BadRequest(
            "a test run does not support delegateDid; fire the trigger `as` the \
             delegated account instead"
                .into(),
        ));
    }

    if let Some(resolved) = resolved {
        // Delegation guard preserved from origin/dev: scripts that run
        // under a `delegateDid` must come from a caller who is an
        // active write-capable delegate of that account, scoped to the
        // calling api_client.
        let delegate_did = input
            .get("delegateDid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if let Some(ref did) = delegate_did {
            let client_key = claims
                .client_key()
                .ok_or_else(|| AppError::Auth("delegation requires DPoP authentication".into()))?;
            let api_client_id = repo::get_dpop_client_id(state, client_key).await?;

            let role = crate::delegation::db::get_delegate_role(
                &state.db,
                state.db_backend,
                did,
                claims.did(),
            )
            .await?
            .ok_or_else(|| AppError::Forbidden("you are not a delegate of this account".into()))?;

            if !role.can_write() {
                return Err(AppError::Forbidden(
                    "your role does not have write access to this account".into(),
                ));
            }

            let stored_client_id =
                crate::delegation::db::get_api_client_id(&state.db, state.db_backend, did)
                    .await?
                    .ok_or_else(|| {
                        AppError::Internal("delegated account missing api_client_id".into())
                    })?;

            if api_client_id != stored_client_id {
                return Err(AppError::Forbidden(
                    "delegation is scoped to a different application".into(),
                ));
            }
        }

        let mut script_input = input.clone();
        if let Some(obj) = script_input.as_object_mut() {
            obj.remove("delegateDid");
        }

        return crate::lua::execute_procedure_script(
            state,
            method,
            claims,
            &script_input,
            params,
            lexicon,
            &resolved,
            None,
            delegate_did.as_deref(),
        )
        .await;
    }

    Err(super::query::no_script("procedure", method))
}
