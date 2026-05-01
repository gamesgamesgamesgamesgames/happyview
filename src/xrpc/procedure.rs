use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::Claims;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::lexicon::ProcedureAction;
use crate::record_refs::sync_refs;
use crate::repo;

pub(crate) async fn handle_procedure(
    state: &AppState,
    method: &str,
    claims: &Claims,
    input: &Value,
    params: &std::collections::HashMap<String, Value>,
    lexicon: &crate::lexicon::ParsedLexicon,
) -> Result<Response, AppError> {
    // Trigger-keyed dispatch: a script bound at `xrpc.procedure:<id>`
    // overrides the default PDS-write flow. The legacy `lexicon.script`
    // column is no longer read.
    let trigger = format!("xrpc.procedure:{}", lexicon.id);
    if let Some(resolved) = crate::lua::resolve(state, &trigger).await {
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
            &resolved.body,
            None,
            delegate_did.as_deref(),
        )
        .await;
    }

    let collection = lexicon.target_collection.as_deref().ok_or_else(|| {
        AppError::BadRequest(format!("{method} has no target_collection configured"))
    })?;

    // If the user authenticated via a DPoP session (has a client_key from DPoP auth),
    // use the DPoP PDS write path. Otherwise, fall back to the atrium OAuth session.
    if let Some(client_key) = claims.client_key() {
        let encryption_key = state
            .config
            .token_encryption_key
            .as_ref()
            .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;

        let api_client_id = repo::get_dpop_client_id(state, client_key).await?;

        let delegate_did = input
            .get("delegateDid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        return handle_dpop_procedure(
            state,
            claims,
            input,
            collection,
            &lexicon.action,
            &api_client_id,
            encryption_key,
            delegate_did.as_deref(),
        )
        .await;
    }

    let session = repo::get_oauth_session(state, claims.did()).await?;

    match &lexicon.action {
        ProcedureAction::Create => {
            handle_create_record(state, claims, input, collection, &session).await
        }
        ProcedureAction::Update => {
            handle_put_record(state, claims, input, collection, &session).await
        }
        ProcedureAction::Delete => {
            handle_delete_record(state, claims, input, collection, &session).await
        }
        ProcedureAction::Upsert => {
            // Backwards-compatible: sniff for `uri` field to decide create vs put.
            let has_uri = input.get("uri").and_then(|v| v.as_str()).is_some();
            if has_uri {
                handle_put_record(state, claims, input, collection, &session).await
            } else {
                handle_create_record(state, claims, input, collection, &session).await
            }
        }
    }
}

async fn handle_create_record(
    state: &AppState,
    claims: &Claims,
    input: &Value,
    collection: &str,
    session: &crate::HappyViewOAuthSession,
) -> Result<Response, AppError> {
    // Build record from input, adding $type
    let mut record = input.clone();
    if let Some(obj) = record.as_object_mut() {
        obj.insert("$type".to_string(), json!(collection));
        // Remove fields that are procedure params, not record fields
        obj.remove("shouldPublish");
    }

    let pds_body = json!({
        "repo": claims.did(),
        "collection": collection,
        "record": record,
    });

    let resp =
        repo::pds_post_json_raw(state, session, "com.atproto.repo.createRecord", &pds_body).await?;

    if resp.status().is_success() {
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AppError::Internal(format!("failed to read PDS response: {e}")))?;

        let pds_result: Value = serde_json::from_slice(&bytes)
            .map_err(|e| AppError::Internal(format!("invalid PDS JSON: {e}")))?;

        if let (Some(uri), Some(cid)) = (
            pds_result.get("uri").and_then(|v| v.as_str()),
            pds_result.get("cid").and_then(|v| v.as_str()),
        ) {
            let backend = state.db_backend;
            let rkey = uri.split('/').next_back().unwrap_or_default();
            let record_str = serde_json::to_string(&record).unwrap_or_default();
            let sql = adapt_sql(
                r#"
                INSERT INTO records (uri, did, collection, rkey, record, cid, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (uri) DO UPDATE
                    SET record = EXCLUDED.record,
                        cid = EXCLUDED.cid
                "#,
                backend,
            );
            let now = now_rfc3339();
            let _ = sqlx::query(&sql)
                .bind(uri)
                .bind(claims.did())
                .bind(collection)
                .bind(rkey)
                .bind(&record_str)
                .bind(cid)
                .bind(&now)
                .execute(&state.db)
                .await;

            let _ = sync_refs(&state.db, uri, collection, &record, backend).await;
        }

        Ok((
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            bytes,
        )
            .into_response())
    } else {
        repo::forward_pds_response(resp).await
    }
}

async fn handle_put_record(
    state: &AppState,
    claims: &Claims,
    input: &Value,
    collection: &str,
    session: &crate::HappyViewOAuthSession,
) -> Result<Response, AppError> {
    let uri = input
        .get("uri")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("missing uri field".into()))?;

    let rkey = uri
        .split('/')
        .next_back()
        .ok_or_else(|| AppError::Internal("invalid AT URI".into()))?;

    // Build record from input, adding $type
    let mut record = input.clone();
    if let Some(obj) = record.as_object_mut() {
        obj.insert("$type".to_string(), json!(collection));
        obj.remove("uri");
        obj.remove("shouldPublish");
    }

    let pds_body = json!({
        "repo": claims.did(),
        "collection": collection,
        "rkey": rkey,
        "record": record,
    });

    let resp =
        repo::pds_post_json_raw(state, session, "com.atproto.repo.putRecord", &pds_body).await?;

    if resp.status().is_success() {
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AppError::Internal(format!("failed to read PDS response: {e}")))?;

        let pds_result: Value = serde_json::from_slice(&bytes)
            .map_err(|e| AppError::Internal(format!("invalid PDS JSON: {e}")))?;

        let cid = pds_result
            .get("cid")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let backend = state.db_backend;
        let record_str = serde_json::to_string(&record).unwrap_or_default();
        let now = now_rfc3339();
        let sql = adapt_sql(
            r#"
            INSERT INTO records (uri, did, collection, rkey, record, cid, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (uri) DO UPDATE
                SET record = EXCLUDED.record,
                    cid = EXCLUDED.cid,
                    indexed_at = ?
            "#,
            backend,
        );
        let _ = sqlx::query(&sql)
            .bind(uri)
            .bind(claims.did())
            .bind(collection)
            .bind(rkey)
            .bind(&record_str)
            .bind(cid)
            .bind(&now)
            .bind(&now)
            .execute(&state.db)
            .await;

        let _ = sync_refs(&state.db, uri, collection, &record, backend).await;

        Ok((
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            bytes,
        )
            .into_response())
    } else {
        repo::forward_pds_response(resp).await
    }
}

async fn handle_delete_record(
    state: &AppState,
    claims: &Claims,
    input: &Value,
    collection: &str,
    session: &crate::HappyViewOAuthSession,
) -> Result<Response, AppError> {
    let uri = input
        .get("uri")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("missing uri field".into()))?;

    let rkey = uri
        .split('/')
        .next_back()
        .ok_or_else(|| AppError::Internal("invalid AT URI".into()))?;

    let pds_body = json!({
        "repo": claims.did(),
        "collection": collection,
        "rkey": rkey,
    });

    let resp =
        repo::pds_post_json_raw(state, session, "com.atproto.repo.deleteRecord", &pds_body).await?;

    if resp.status().is_success() {
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AppError::Internal(format!("failed to read PDS response: {e}")))?;

        let backend = state.db_backend;
        let sql = adapt_sql("DELETE FROM records WHERE uri = ?", backend);
        let _ = sqlx::query(&sql).bind(uri).execute(&state.db).await;

        Ok((
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            bytes,
        )
            .into_response())
    } else {
        repo::forward_pds_response(resp).await
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_dpop_procedure(
    state: &AppState,
    claims: &Claims,
    input: &Value,
    collection: &str,
    action: &ProcedureAction,
    api_client_id: &str,
    encryption_key: &[u8; 32],
    delegate_did: Option<&str>,
) -> Result<Response, AppError> {
    // If delegating, verify the caller has write access and resolve the
    // api_client_id that owns the delegated session.
    let (target_did, effective_api_client_id) = if let Some(did) = delegate_did {
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

        (did, stored_client_id)
    } else {
        (claims.did(), api_client_id.to_string())
    };

    // Strip delegateDid from input — it's a control field, not record data
    let mut input = input.clone();
    if let Some(obj) = input.as_object_mut() {
        obj.remove("delegateDid");
    }

    let (xrpc_method, pds_body) = match action {
        ProcedureAction::Create => {
            let mut record = input.clone();
            if let Some(obj) = record.as_object_mut() {
                obj.insert("$type".to_string(), json!(collection));
                obj.remove("shouldPublish");
                obj.remove("delegateDid");
            }
            (
                "com.atproto.repo.createRecord",
                json!({
                    "repo": target_did,
                    "collection": collection,
                    "record": record,
                }),
            )
        }
        ProcedureAction::Update => {
            let uri = input
                .get("uri")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError::BadRequest("missing uri field".into()))?;
            let rkey = uri
                .split('/')
                .next_back()
                .ok_or_else(|| AppError::Internal("invalid AT URI".into()))?;
            let mut record = input.clone();
            if let Some(obj) = record.as_object_mut() {
                obj.insert("$type".to_string(), json!(collection));
                obj.remove("uri");
                obj.remove("shouldPublish");
                obj.remove("delegateDid");
            }
            (
                "com.atproto.repo.putRecord",
                json!({
                    "repo": target_did,
                    "collection": collection,
                    "rkey": rkey,
                    "record": record,
                }),
            )
        }
        ProcedureAction::Delete => {
            let uri = input
                .get("uri")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError::BadRequest("missing uri field".into()))?;
            let rkey = uri
                .split('/')
                .next_back()
                .ok_or_else(|| AppError::Internal("invalid AT URI".into()))?;
            (
                "com.atproto.repo.deleteRecord",
                json!({
                    "repo": target_did,
                    "collection": collection,
                    "rkey": rkey,
                }),
            )
        }
        ProcedureAction::Upsert => {
            let has_uri = input.get("uri").and_then(|v| v.as_str()).is_some();
            if has_uri {
                let uri = input["uri"].as_str().unwrap();
                let rkey = uri
                    .split('/')
                    .next_back()
                    .ok_or_else(|| AppError::Internal("invalid AT URI".into()))?;
                let mut record = input.clone();
                if let Some(obj) = record.as_object_mut() {
                    obj.insert("$type".to_string(), json!(collection));
                    obj.remove("uri");
                    obj.remove("shouldPublish");
                    obj.remove("delegateDid");
                }
                (
                    "com.atproto.repo.putRecord",
                    json!({
                        "repo": target_did,
                        "collection": collection,
                        "rkey": rkey,
                        "record": record,
                    }),
                )
            } else {
                let mut record = input.clone();
                if let Some(obj) = record.as_object_mut() {
                    obj.insert("$type".to_string(), json!(collection));
                    obj.remove("shouldPublish");
                    obj.remove("delegateDid");
                }
                (
                    "com.atproto.repo.createRecord",
                    json!({
                        "repo": target_did,
                        "collection": collection,
                        "record": record,
                    }),
                )
            }
        }
    };

    let resp = crate::oauth::pds_write::dpop_pds_post(
        &state.http,
        &state.db,
        state.db_backend,
        encryption_key,
        &state.oauth,
        &state.config.plc_url,
        &effective_api_client_id,
        target_did,
        xrpc_method,
        &pds_body,
    )
    .await?;

    repo::forward_pds_response(resp).await
}
