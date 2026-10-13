//! `/admin/scripts` CRUD — trigger-keyed scripts.
//!
//! Each script row's `id` IS its trigger string (e.g.
//! `record.create:com.example.thing`, `xrpc.query:com.foo.list`,
//! `labeler.apply:_actor`). The dispatcher in [`crate::lua::scripts`]
//! looks up scripts by id at firing time; this admin surface lets
//! operators CRUD those rows.
//!
//! Validation:
//! - On create / patch the body is checked by the interpreter the script_type
//!   names, through its `validate` export. A body it refuses is rejected at
//!   write-time with a 400 carrying what it said.
//! - A script_type no installed interpreter claims is rejected with a 400
//!   (see [`validate_body_for_type`]).
//! - A Lua body that still reaches a removed global is refused with a 400
//!   (see [`refuse_unmigrated`]), ahead of the interpreter's check.
//! - The trigger id is parsed against
//!   [`crate::lua::ParsedTrigger::parse`]; unknown prefixes / invalid
//!   NSIDs are rejected at write-time with a 400.
//!
//! Permissions: `scripts:read` for GETs; `scripts:manage` for the
//! mutating endpoints.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::AppState;
use crate::codemod::{self, ScriptKind};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::lua::{NATIVE_LANGUAGE, ParsedTrigger};
use crate::plugin::{ScriptErrorKind, ScriptValidateError};
use crate::script::checks::{self, CheckError, InterpreterCheck, MAX_DESCRIPTION_LEN};

use super::auth::UserAuth;
use super::permissions::Permission;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One script as the catalog answers for it — what GET endpoints return.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ScriptResponse {
    /// The trigger id; identifies the row.
    pub id: String,
    pub script_type: String,
    pub body: String,
    pub description: Option<String>,
    pub outbound_xrpcs: Option<Vec<String>>,
    /// `None` for a project script, which has no row.
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    /// The layer answering for this trigger.
    pub layer: crate::catalog::Layer,
    /// Project-relative file path, for a project script only.
    pub path: Option<String>,
    /// A dashboard row sits under this project file and answers again when
    /// the file goes.
    pub shadows_stored: bool,
    /// `false` when this row's own trigger id would be refused if submitted
    /// today — the NSID rules tightened after it was created. Such a script
    /// still fires and can still be edited, but it cannot be recreated with
    /// the same id after deletion.
    pub recreatable: bool,
    /// v2 globals this script still references, for the kind its trigger id
    /// implies. Empty once migrated. Always empty for a non-Lua script — the
    /// v2/v3 contract this names is Lua's.
    ///
    /// The single entry `"unparseable"` is not a global, as
    /// `codemod::needs_migration` explains.
    pub needs_migration: Vec<String>,
    /// `false` when no installed interpreter claims this row's language, so
    /// the script is stored and inert. A client says so rather than showing
    /// the row as broken: the row is intact and installing the interpreter is
    /// the whole fix.
    pub runnable: bool,
}

impl ScriptResponse {
    /// Builds a response from a listing.
    ///
    /// `recreatable` and `outbound_xrpcs` are derived here rather than passed
    /// in, so a new call site cannot forget them. `recreatable` is `false`
    /// when the row's own trigger id would be refused if submitted today.
    ///
    /// `installed` is passed in because it is one read of the plugin registry
    /// for a whole listing rather than one per row.
    fn from_listing(l: crate::catalog::ScriptListing, installed: &HashSet<String>) -> Self {
        let recreatable = ParsedTrigger::parse(&l.id).is_ok();
        let needs_migration = if l.script_type == NATIVE_LANGUAGE {
            codemod::needs_migration(&l.body, ScriptKind::from_trigger_id(&l.id))
                .into_iter()
                .map(str::to_string)
                .collect()
        } else {
            Vec::new()
        };
        Self {
            runnable: installed.contains(&l.script_type),
            path: l.path.as_deref().map(crate::catalog::path_string),
            layer: l.layer,
            shadows_stored: l.shadows_stored,
            id: l.id,
            script_type: l.script_type,
            body: l.body,
            description: l.description,
            outbound_xrpcs: l.outbound_xrpcs,
            created_at: l.created_at,
            updated_at: l.updated_at,
            recreatable,
            needs_migration,
        }
    }
}

/// Body for `POST /admin/scripts` (create or replace by `id`).
#[derive(Debug, Deserialize)]
pub(super) struct UpsertBody {
    pub id: String,
    /// Defaults to [`NATIVE_LANGUAGE`] server-side if omitted.
    #[serde(default)]
    pub script_type: Option<String>,
    pub body: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Body for `PATCH /admin/scripts/{id}`. All fields optional.
#[derive(Debug, Deserialize)]
pub(super) struct PatchBody {
    #[serde(default)]
    pub script_type: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    /// Set to `Some(None)` to clear via JSON `null`.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub description: Option<Option<String>>,
}

/// Three-state field deserializer: missing → `None`, `null` → `Some(None)`,
/// string → `Some(Some(s))`. Lets PATCH distinguish "leave as-is" from
/// "clear to NULL".
fn deserialize_optional_field<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: Option<String> = Option::deserialize(d)?;
    Ok(Some(v))
}

/// Body for `POST /admin/scripts/{id}/codemod`. Omitted entirely for a
/// preview.
#[derive(Debug, Deserialize, Default)]
pub(super) struct CodemodBody {
    #[serde(default)]
    pub apply: bool,
    /// Overrides the marker guard on apply — see `codemod_apply`.
    #[serde(default)]
    pub allow_markers: bool,
    /// Text to rewrite in place of the stored body. Preview only.
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct CodemodNote {
    pub line: usize,
    pub message: String,
}

impl From<codemod::Note> for CodemodNote {
    fn from(note: codemod::Note) -> Self {
        Self {
            line: note.line,
            message: note.message,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct CodemodResponse {
    pub source: String,
    pub notes: Vec<CodemodNote>,
    pub changed: bool,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(super) struct ListQuery {
    pub suffix: Option<String>,
}

/// `GET /admin/scripts` — list all rows. Clients group by trigger family
/// in the UI. Optional `?suffix=<nsid>` filters to scripts whose id
/// ends with `:<suffix>`.
pub(super) async fn list(
    State(state): State<AppState>,
    auth: UserAuth,
    axum::extract::Query(query): axum::extract::Query<ListQuery>,
) -> Result<Json<Vec<ScriptResponse>>, AppError> {
    auth.require(Permission::ScriptsRead).await?;

    let installed = installed_languages(&state).await;
    let scripts = state
        .scripts
        .list(query.suffix.as_deref())
        .await?
        .into_iter()
        .map(|l| ScriptResponse::from_listing(l, &installed))
        .collect();
    Ok(Json(scripts))
}

/// `GET /admin/scripts/{id}` — fetch one row.
pub(super) async fn get(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<ScriptResponse>, AppError> {
    auth.require(Permission::ScriptsRead).await?;
    fetch_one(&state, &id).await.map(Json)
}

/// `POST /admin/scripts` — create or replace a row by `id`. Returns the
/// upserted row. Status `201 Created` for a new row, `200 OK` for an
/// update.
pub(super) async fn upsert(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<UpsertBody>,
) -> Result<(StatusCode, Json<ScriptResponse>), AppError> {
    auth.require(Permission::ScriptsManage).await?;
    state.scripts.check_writable(&body.id)?;

    // Validate the trigger id grammar up-front (400 with a clear message).
    let _trigger = ParsedTrigger::parse(&body.id).map_err(AppError::BadRequest)?;

    if let Some(ref desc) = body.description
        && desc.len() > MAX_DESCRIPTION_LEN
    {
        return Err(AppError::BadRequest(format!(
            "description must be at most {MAX_DESCRIPTION_LEN} characters"
        )));
    }

    let script_type = body
        .script_type
        .unwrap_or_else(|| NATIVE_LANGUAGE.to_string());
    refuse_unmigrated(&body.id, &script_type, &body.body)?;
    validate_body_for_type(&state, &body.body, &script_type).await?;

    let backend = state.db_backend;
    let description = body.description.as_deref().filter(|s| !s.is_empty());
    let was_new = state
        .scripts
        .put_manual(
            &state.db,
            backend,
            crate::catalog::ScriptWrite {
                id: body.id.clone(),
                script_type: script_type.clone(),
                body: body.body.clone(),
                description: description.map(str::to_string),
            },
        )
        .await?;

    log_event(
        &state.db,
        EventLog {
            event_type: if was_new {
                "script.created".to_string()
            } else {
                "script.updated".to_string()
            },
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(body.id.clone()),
            detail: serde_json::json!({
                "script_type": script_type.as_str(),
            }),
        },
        backend,
    )
    .await;

    let row = fetch_one(&state, &body.id).await?;
    let status = if was_new {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(row)))
}

/// `PATCH /admin/scripts/{id}` — partial update. At least one of
/// `script_type` / `body` / `description` must be present.
pub(super) async fn patch(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    Json(body): Json<PatchBody>,
) -> Result<Json<ScriptResponse>, AppError> {
    auth.require(Permission::ScriptsManage).await?;
    state.scripts.check_writable(&id)?;

    if body.script_type.is_none() && body.body.is_none() && body.description.is_none() {
        return Err(AppError::BadRequest(
            "patch requires at least one of: script_type, body, description".into(),
        ));
    }
    // Patching a body or script_type? We need a body to validate against
    // the (possibly new) language. Patching script_type alone is
    // ambiguous (we'd be validating the existing body against the new
    // language without re-checking it makes sense), so reject it.
    if body.script_type.is_some() && body.body.is_none() {
        return Err(AppError::BadRequest(
            "patching script_type requires body alongside (so the server can re-validate)".into(),
        ));
    }
    // Existence check + fetch current values. Ahead of the body check,
    // because a patch that does not name a language is a patch under the
    // stored one, and the alternative — the compiled-in default — hands one
    // interpreter's body to another.
    let existing = fetch_one(&state, &id).await?;

    if let Some(ref new_body) = body.body {
        let lang = body.script_type.as_deref().unwrap_or(&existing.script_type);
        refuse_unmigrated(&id, lang, new_body)?;
        validate_body_for_type(&state, new_body, lang).await?;
    }
    if let Some(Some(ref desc)) = body.description
        && desc.len() > MAX_DESCRIPTION_LEN
    {
        return Err(AppError::BadRequest(format!(
            "description must be at most {MAX_DESCRIPTION_LEN} characters"
        )));
    }

    let backend = state.db_backend;
    let new_script_type = body.script_type.unwrap_or(existing.script_type);
    let new_body = body.body.unwrap_or(existing.body);
    let new_description = match body.description {
        Some(desc_opt) => desc_opt,
        None => existing.description,
    };

    state
        .scripts
        .put_manual(
            &state.db,
            backend,
            crate::catalog::ScriptWrite {
                id: id.clone(),
                script_type: new_script_type.clone(),
                body: new_body,
                description: new_description,
            },
        )
        .await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "script.updated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id.clone()),
            detail: serde_json::json!({
                "script_type": new_script_type,
            }),
        },
        backend,
    )
    .await;

    let row = fetch_one(&state, &id).await?;
    Ok(Json(row))
}

/// `DELETE /admin/scripts/{id}` — remove a row. 204 on success, 404 if
/// no row matched.
pub(super) async fn delete(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::ScriptsManage).await?;

    let backend = state.db_backend;
    if !state.scripts.remove(&state.db, backend, &id).await? {
        return Err(AppError::NotFound(format!("script '{id}' not found")));
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "script.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id),
            detail: serde_json::json!({}),
        },
        backend,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /admin/scripts/{id}/codemod` — rewrite the row's body onto the v3
/// `handle(input, ctx)` contract. Preview by default; `{"apply": true}`
/// stores the result, which needs `scripts:manage` since it is a write —
/// `scripts:read` alone only ever previews.
///
/// A rewrite that would store something new and still leaves `-- codemod:`
/// markers behind is refused (409) unless `allow_markers` is also set: a
/// marked construct still runs on the old globals for those lines, so
/// storing it would make the script look migrated when it is not.
///
/// `source` previews the rewrite of text the table does not hold. The save
/// paths refuse an unmigrated body, so the editor needs the rewrite of what
/// it is showing, and a script that has never been saved has no row to read:
/// the id then supplies only the kind.
pub(super) async fn codemod_apply(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    body: Option<Json<CodemodBody>>,
) -> Result<Json<CodemodResponse>, AppError> {
    auth.require(Permission::ScriptsRead).await?;

    let CodemodBody {
        apply,
        allow_markers,
        source: draft,
    } = body.map(|Json(b)| b).unwrap_or_default();
    let kind = ScriptKind::from_trigger_id(&id);

    if let Some(draft) = draft {
        if apply {
            return Err(AppError::BadRequest(
                "apply rewrites the stored script; a supplied source is preview-only".to_string(),
            ));
        }
        let rewrite = rewrite_lua(&draft, kind)?;
        return Ok(Json(CodemodResponse {
            changed: rewrite.source != draft,
            source: rewrite.source,
            notes: rewrite.notes.into_iter().map(CodemodNote::from).collect(),
        }));
    }

    // Ahead of the fetch and the language check: a project-owned script is
    // refused as such whatever its language, not as "not Lua".
    if apply {
        auth.require(Permission::ScriptsManage).await?;
        state.scripts.check_writable(&id)?;
    }

    let existing = fetch_one(&state, &id).await?;
    if existing.script_type != NATIVE_LANGUAGE {
        return Err(AppError::BadRequest(
            "codemod applies to Lua scripts".to_string(),
        ));
    }

    let rewrite = rewrite_lua(&existing.body, kind)?;
    let changed = rewrite.source != existing.body;
    let marker_count = rewrite.notes.len();

    // The guard is about what gets stored, so it only fires where something
    // would be: re-applying a rewrite that has already landed writes nothing,
    // and refusing it would make a repeated call look like a new problem.
    if apply && changed && marker_count > 0 && !allow_markers {
        return Err(AppError::Conflict(format!(
            "codemod left {marker_count} marker(s) in place; refusing to apply without allow_markers"
        )));
    }

    if apply && changed {
        let backend = state.db_backend;
        state
            .scripts
            .put_manual(
                &state.db,
                backend,
                crate::catalog::ScriptWrite {
                    id: id.clone(),
                    script_type: existing.script_type.clone(),
                    body: rewrite.source.clone(),
                    description: existing.description.clone(),
                },
            )
            .await?;

        log_event(
            &state.db,
            EventLog {
                event_type: "script.codemod_applied".to_string(),
                severity: Severity::Info,
                actor_did: Some(auth.did.clone()),
                subject: Some(id.clone()),
                detail: serde_json::json!({
                    "script_type": existing.script_type,
                    "markers": marker_count,
                }),
            },
            backend,
        )
        .await;
    }

    Ok(Json(CodemodResponse {
        source: rewrite.source,
        notes: rewrite.notes.into_iter().map(CodemodNote::from).collect(),
        changed,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The script answering for `id`; 404 if none.
async fn fetch_one(state: &AppState, id: &str) -> Result<ScriptResponse, AppError> {
    let listing = state
        .scripts
        .get(id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("script '{id}' not found")))?;
    Ok(ScriptResponse::from_listing(
        listing,
        &installed_languages(state).await,
    ))
}

fn rewrite_lua(source: &str, kind: ScriptKind) -> Result<codemod::Rewrite, AppError> {
    codemod::rewrite(source, kind)
        .map_err(|e| AppError::BadRequest(format!("script did not parse as Lua: {e}")))
}

/// Refuses a Lua body that still reaches a removed global, naming each one in
/// `needs_migration`'s order. Such a script would save cleanly and then fail
/// on its first run, far from the edit that caused it.
///
/// It runs ahead of the interpreter's check, which loads the chunk under the
/// removed-name guard, so a read at file scope would otherwise answer as a
/// compilation failure, which offers no way to the codemod.
///
/// The codemod's own apply does not come through here: what it may leave
/// behind are marked constructs, the markers are comments saying what to
/// finish, and `allow_markers` is the operator's consent to store them.
///
/// A body the scanner cannot read is left to the compile check, which owns
/// the message for a script that does not parse.
fn refuse_unmigrated(id: &str, script_type: &str, body: &str) -> Result<(), AppError> {
    let remaining = checks::unmigrated_globals(id, script_type, body);
    if remaining.is_empty() {
        return Ok(());
    }
    Err(AppError::UnmigratedScript(remaining))
}

/// Validate the script body against its declared language, rejecting an
/// invalid body with a 400 at write-time.
///
/// The check belongs to the interpreter that would run the body, so the
/// editor and a run agree about what the language accepts by asking the same
/// thing. A language no installed interpreter claims is refused outright:
/// nothing could check the body and nothing could run it, so storing it would
/// accept a script on no authority at all. That refusal borrows the
/// dispatcher's own sentence, since the gap and the fix are the same ones a
/// run reports.
///
/// An interpreter that cannot answer at all is a 500: the body may be
/// perfectly good, and refusing it would blame the operator for a broken
/// plugin.
async fn validate_body_for_type(
    state: &AppState,
    body: &str,
    language: &str,
) -> Result<(), AppError> {
    match checks::check_with_interpreter(
        &state.plugin_registry,
        &state.plugin_executor(),
        language,
        body,
    )
    .await
    {
        Ok(InterpreterCheck::Valid) => Ok(()),
        Ok(InterpreterCheck::Refused(errors)) => {
            Err(AppError::BadRequest(refusal_message(&errors)))
        }
        Err(CheckError::NoInterpreter(message)) => Err(AppError::BadRequest(message)),
        Err(e @ CheckError::InterpreterFailed { .. }) => Err(AppError::Internal(e.to_string())),
    }
}

/// What a refusal says, from what the interpreter reported.
///
/// A missing `handle` is a whole sentence about the script's shape, so it
/// stands alone. Everything else is a failure at a position, and the line is
/// the half an operator needs: an interpreter reports it as a field rather
/// than inside the message, since the chunk name it would otherwise sit
/// behind names nothing an operator can open. An interpreter free to report
/// several reasons has all of them said rather than the rest dropped.
fn refusal_message(errors: &[ScriptValidateError]) -> String {
    if errors.is_empty() {
        return "the interpreter refused the script without saying why".to_string();
    }
    errors
        .iter()
        .map(|error| match (error.kind, error.line) {
            (ScriptErrorKind::MissingHandle, _) => error.message.clone(),
            (_, Some(line)) => {
                format!(
                    "script compilation failed at line {line}: {}",
                    error.message
                )
            }
            (_, None) => format!("script compilation failed: {}", error.message),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The languages installed interpreters claim.
async fn installed_languages(state: &AppState) -> HashSet<String> {
    state
        .plugin_registry
        .list_by_type(crate::plugin::PluginType::Interpreter)
        .await
        .iter()
        .filter_map(|p| p.language_id().map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // `ScriptResponse::from_listing` is the only place `recreatable` and
    // `needs_migration` are derived — both HTTP construction sites (`list`
    // and `fetch_one`) go through it. These exercise it directly, since
    // exercising the full HTTP handlers needs a database (see
    // `tests/e2e_scripts.rs`, which is `#[ignore]`-gated).

    fn lua_installed() -> HashSet<String> {
        HashSet::from([NATIVE_LANGUAGE.to_string()])
    }

    fn listing(id: &str, script_type: &str, body: &str) -> crate::catalog::ScriptListing {
        crate::catalog::ScriptListing {
            id: id.into(),
            layer: crate::catalog::Layer::Manual,
            path: None,
            shadows_stored: false,
            script_type: script_type.into(),
            body: body.into(),
            description: None,
            outbound_xrpcs: None,
            created_at: Some("2026-01-01T00:00:00+00:00".into()),
            updated_at: Some("2026-01-01T00:00:00+00:00".into()),
        }
    }

    /// `runnable` answers for the instance, not for the row: the same stored
    /// script is runnable or inert depending only on what is installed.
    #[test]
    fn from_listing_reads_runnable_off_the_installed_interpreters() {
        let row = |installed: &HashSet<String>| {
            ScriptResponse::from_listing(
                listing(
                    "xrpc.query:com.example.list",
                    "typescript",
                    "function handle() {}",
                ),
                installed,
            )
            .runnable
        };
        assert!(!row(&lua_installed()));
        assert!(row(&HashSet::from([
            "lua".to_string(),
            "typescript".to_string()
        ])));
    }

    #[test]
    fn from_listing_marks_a_valid_trigger_id_recreatable() {
        let r = ScriptResponse::from_listing(
            listing(
                "record.create:com.example.thing",
                "lua",
                "function handle() end",
            ),
            &lua_installed(),
        );
        assert!(r.recreatable);
    }

    #[test]
    fn from_listing_marks_a_legacy_trigger_id_not_recreatable() {
        // Hyphen in the name segment: accepted before the NSID consolidation,
        // refused now. The script still fires, but cannot be recreated.
        let r = ScriptResponse::from_listing(
            listing(
                "xrpc.query:com.example.get-photos",
                "lua",
                "function handle() end",
            ),
            &lua_installed(),
        );
        assert!(!r.recreatable);
    }

    /// A job saved before `.test` was refused keeps its row: it still
    /// resolves by exact id and still lists, it just can't be recreated.
    #[test]
    fn from_listing_marks_a_job_ending_in_test_not_recreatable() {
        let r = ScriptResponse::from_listing(
            listing("job.run:nightly.test", "lua", "function handle() end"),
            &lua_installed(),
        );
        assert!(!r.recreatable);
        assert!(r.runnable);
    }

    /// The outbound list reaches the response as the catalog parsed it. The
    /// corrupt-column case is pinned where the parse lives, by
    /// `a_corrupt_outbound_column_reads_as_none_rather_than_failing`.
    #[test]
    fn from_listing_carries_outbound_xrpcs_through() {
        let mut l = listing("record.create:com.example.thing", "lua", "");
        l.outbound_xrpcs = Some(vec!["com.example.foo".into()]);
        let r = ScriptResponse::from_listing(l, &lua_installed());
        assert_eq!(
            r.outbound_xrpcs.as_deref(),
            Some(&["com.example.foo".to_string()][..])
        );
    }

    #[test]
    fn from_listing_carries_the_layer_through() {
        let mut l = listing(
            "xrpc.query:com.example.list",
            "lua",
            "function handle() end",
        );
        l.layer = crate::catalog::Layer::Project;
        l.path = Some(std::path::PathBuf::from(
            "src/lexicons/com/example/list.lua",
        ));
        l.shadows_stored = true;
        l.created_at = None;
        l.updated_at = None;
        let r = ScriptResponse::from_listing(l, &lua_installed());
        assert_eq!(r.layer, crate::catalog::Layer::Project);
        assert_eq!(r.path.as_deref(), Some("src/lexicons/com/example/list.lua"));
        assert!(r.shadows_stored);
        assert_eq!(r.created_at, None);
        assert_eq!(r.updated_at, None);
    }

    #[test]
    fn from_listing_reports_needs_migration_for_a_lua_script() {
        let r = ScriptResponse::from_listing(
            listing(
                "xrpc.query:com.example.list",
                "lua",
                "function handle()\n  return params.q\nend\n",
            ),
            &lua_installed(),
        );
        assert_eq!(r.needs_migration, vec!["params".to_string()]);
    }

    #[test]
    fn from_listing_needs_migration_is_empty_for_a_non_lua_script() {
        let r = ScriptResponse::from_listing(
            listing(
                "xrpc.query:com.example.list",
                "javascript",
                "function handle() { return params.q; }",
            ),
            &lua_installed(),
        );
        assert!(r.needs_migration.is_empty());
    }

    fn refusal(id: &str, script_type: &str, body: &str) -> Option<String> {
        match refuse_unmigrated(id, script_type, body) {
            Ok(()) => None,
            Err(AppError::UnmigratedScript(globals)) => Some(globals.join(", ")),
            Err(other) => panic!("expected an unmigrated-script refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_unmigrated_body_is_refused_naming_its_globals_in_order() {
        let body = "function handle()\n  log(now())\n  return db.get(params.uri)\nend\n";
        assert_eq!(
            refusal("xrpc.query:com.example.list", "lua", body).as_deref(),
            Some("db, log, now, params")
        );
    }

    #[test]
    fn the_refusal_follows_the_kind_the_trigger_id_implies() {
        let body = "function handle()\n  return { v = val }\nend\n";
        assert_eq!(
            refusal("labeler.apply:_actor", "lua", body).as_deref(),
            Some("val")
        );
        assert_eq!(refusal("xrpc.query:com.example.list", "lua", body), None);
    }

    #[test]
    fn a_migrated_body_is_not_refused() {
        let body = "local db = require(\"happyview.db\")\n\
                    function handle(input, ctx)\n  return db.get(input.uri)\nend\n";
        assert_eq!(refusal("xrpc.query:com.example.list", "lua", body), None);
    }

    #[test]
    fn a_non_lua_body_is_not_checked() {
        let body = "function handle() { return params.q; }";
        assert_eq!(
            refusal("xrpc.query:com.example.list", "javascript", body),
            None
        );
    }

    #[test]
    fn a_body_the_scanner_cannot_read_is_not_called_unmigrated() {
        assert_eq!(
            refusal("xrpc.query:com.example.list", "lua", "function handle( end"),
            None
        );
    }

    fn validate_error(
        kind: ScriptErrorKind,
        line: Option<u32>,
        message: &str,
    ) -> ScriptValidateError {
        ScriptValidateError {
            kind,
            line,
            message: message.to_string(),
        }
    }

    /// What each kind an interpreter can report reads as, with a line and
    /// without one. This runs wherever the crate builds, so it is what holds
    /// the rendering while the real interpreter's own refusals are asserted
    /// only where it is installed.
    ///
    /// The rows are checked against `ScriptErrorKind::ALL`, so a new kind
    /// fails here rather than falling outside a test that claims to cover
    /// every one. A row's message is the sentence that kind really carries,
    /// since the sentence is half of what an operator reads.
    #[test]
    fn each_kind_is_rendered_as_the_operator_reads_it() {
        let rows = [
            (
                ScriptErrorKind::Syntax,
                "<name> expected near <eof>",
                "script compilation failed at line 7: <name> expected near <eof>",
                "script compilation failed: <name> expected near <eof>",
            ),
            // A read of a removed global fails while the chunk loads, which
            // is a runtime failure rather than a syntax one.
            (
                ScriptErrorKind::Runtime,
                "the 'input' global was removed in v3",
                "script compilation failed at line 7: the 'input' global was removed in v3",
                "script compilation failed: the 'input' global was removed in v3",
            ),
            (
                ScriptErrorKind::Timeout,
                "script exceeded its instruction limit",
                "script compilation failed at line 7: script exceeded its instruction limit",
                "script compilation failed: script exceeded its instruction limit",
            ),
            (
                ScriptErrorKind::Memory,
                "not enough memory",
                "script compilation failed at line 7: not enough memory",
                "script compilation failed: not enough memory",
            ),
            // A whole sentence about the script's shape, so it stands alone
            // and a line would say nothing about where to look.
            (
                ScriptErrorKind::MissingHandle,
                "script must define a handle() function",
                "script must define a handle() function",
                "script must define a handle() function",
            ),
        ];

        let covered: Vec<ScriptErrorKind> = rows.iter().map(|(kind, ..)| *kind).collect();
        assert_eq!(
            covered,
            ScriptErrorKind::ALL,
            "every kind an interpreter can report needs a row here"
        );

        for (kind, message, with_line, without_line) in rows {
            assert_eq!(
                refusal_message(&[validate_error(kind, Some(7), message)]),
                with_line,
                "{kind:?} with a line"
            );
            assert_eq!(
                refusal_message(&[validate_error(kind, None, message)]),
                without_line,
                "{kind:?} with no line"
            );
        }
    }

    /// Two shapes no interpreter in the suite produces, and both are shapes
    /// the contract permits: an interpreter may report several reasons, and
    /// may report none at all.
    #[test]
    fn every_reason_an_interpreter_gave_is_said() {
        assert_eq!(
            refusal_message(&[
                validate_error(ScriptErrorKind::Syntax, Some(3), "unexpected ')'"),
                validate_error(ScriptErrorKind::MissingHandle, None, "no handle"),
            ]),
            "script compilation failed at line 3: unexpected ')'; no handle"
        );
        assert_eq!(
            refusal_message(&[]),
            "the interpreter refused the script without saying why"
        );
    }
}
