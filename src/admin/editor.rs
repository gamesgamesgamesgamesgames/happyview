//! `GET /admin/editor/declarations`: the editor declarations `happyview types`
//! writes for a project, rendered for the whole instance, so the dashboard's
//! script editor completes from the same files an editor reads.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use happyview_plugin_sdk::declarations::DeclarationFile;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::auth::UserAuth;
use super::permissions::Permission;
use crate::AppState;
use crate::error::AppError;
use crate::plugin::PluginType;
use crate::plugin::prepared::ArtifactStamp;
use crate::project::declarations::command::describe;

#[derive(Deserialize)]
pub(super) struct DeclarationsQuery {
    language: String,
}

#[derive(Serialize, Clone)]
pub(super) struct DeclarationsResponse {
    language_id: String,
    files: Vec<DeclarationFile>,
    notes: Vec<String>,
}

/// A rendering and the key it was rendered for.
type Rendered = (String, Arc<DeclarationsResponse>);

/// The last rendering per language. One entry per installed language, so it
/// needs no other bound.
static RENDERED: LazyLock<Mutex<HashMap<String, Rendered>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) async fn declarations(
    State(state): State<AppState>,
    auth: UserAuth,
    headers: HeaderMap,
    Query(query): Query<DeclarationsQuery>,
) -> Result<Response, AppError> {
    auth.require(Permission::ScriptsRead).await?;

    let interpreter = state
        .plugin_registry
        .list_by_type(PluginType::Interpreter)
        .await
        .into_iter()
        .find(|p| p.language_id() == Some(query.language.as_str()))
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "no interpreter for the language '{}'",
                query.language
            ))
        })?;

    let executor = state.plugin_executor();
    let lexicons = state.lexicons.all();
    let mut notes = Vec::new();
    let input = describe(
        &state.plugin_registry,
        &executor,
        lexicons.iter().map(|l| &l.raw),
        &mut notes,
    )
    .await;
    let input_sha256 = hex::encode(Sha256::digest(
        serde_json::to_vec(&input).unwrap_or_default(),
    ));
    let key = format!(
        "{}-{}",
        ArtifactStamp::of(&interpreter).version,
        &input_sha256[..16]
    );
    let etag = format!("\"{key}\"");

    let cached = RENDERED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&query.language)
        .filter(|(k, _)| *k == key)
        .map(|(_, r)| r.clone());
    if cached.is_some()
        && headers
            .get(header::IF_NONE_MATCH)
            .is_some_and(|v| v.as_bytes() == etag.as_bytes())
    {
        return Ok(with_etag(StatusCode::NOT_MODIFIED.into_response(), &etag));
    }
    let response = match cached {
        Some(response) => response,
        None => {
            let files = match executor.declarations_for(&interpreter, &input).await {
                Ok(Some(declarations)) => declarations.files,
                Ok(None) => {
                    notes.push(format!(
                        "{} needs happyview-plugin-sdk 0.4.0 or later for types",
                        interpreter.info.id
                    ));
                    Vec::new()
                }
                Err(e) => {
                    return Err(AppError::BadGateway(format!(
                        "{} could not render declarations: {e}",
                        interpreter.info.id
                    )));
                }
            };
            let response = Arc::new(DeclarationsResponse {
                language_id: query.language.clone(),
                files,
                notes,
            });
            RENDERED
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(query.language.clone(), (key, response.clone()));
            response
        }
    };
    Ok(with_etag(
        Json(response.as_ref().clone()).into_response(),
        &etag,
    ))
}

fn with_etag(mut response: Response, etag: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}
