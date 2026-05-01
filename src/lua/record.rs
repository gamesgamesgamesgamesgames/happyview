use futures_util::future::try_join_all;
use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::AppState;
use crate::auth::Claims;
use crate::db::{adapt_sql, now_rfc3339};
use crate::record_refs::sync_refs;
use crate::repo::PdsAuth;

use super::tid::generate_tid;

const INTERNAL_FIELDS: &[&str] = &[
    "_collection",
    "_uri",
    "_cid",
    "_schema",
    "_key_type",
    "_rkey",
    "_repo_override",
];

/// Error message returned when a script calls a PDS-touching method
/// (`:save()` / `:delete()` / `Record.save_all`) from a context without
/// caller credentials — e.g. label scripts, record-event scripts, or
/// query handlers.
const NO_PDS_AUTH_MSG: &str = "no PDS auth in this script context — \
    use :save_local() / :delete_local() / Record.delete_local(uri) for local-only mutation";

/// Register the `Record` global with only the local-only surface
/// (`Record.load`, `:save_local`, `:delete_local`, `Record.delete_local`).
/// PDS-touching methods (`:save`, `:delete`, `Record.save_all`) are still
/// exposed but error with [`NO_PDS_AUTH_MSG`] when called.
///
/// This is the entry point for label scripts, record-event scripts, and
/// query handlers — contexts that have no caller credentials to round-trip
/// records through a PDS.
pub fn register_record_api_no_auth(lua: &Lua, state: Arc<AppState>) -> LuaResult<()> {
    register_record_api(lua, state, None, None, None)
}

/// Register the `Record` global constructor and static methods.
///
/// `claims` / `pds_auth` are optional: when both are `Some`, the full
/// surface (`:save()`, `:delete()`, `Record.save_all`) round-trips through
/// the PDS. When either is `None` (label scripts, record-event scripts,
/// query handlers), only the local-only methods are usable
/// (`:save_local()`, `:delete_local()`, `Record.delete_local(uri)`); the
/// PDS-touching methods error with [`NO_PDS_AUTH_MSG`]. Most callers want
/// [`register_record_api_no_auth`] instead — this lower-level entry point
/// exposes the internal `PdsAuth` type and is only public to the crate.
///
/// When `delegate_did` is `Some`, record writes default to the delegate's
/// repo instead of the caller's DID. Scripts can still override via
/// `record:set_repo()`.
pub(crate) fn register_record_api(
    lua: &Lua,
    state: Arc<AppState>,
    claims: Option<Arc<Claims>>,
    pds_auth: Option<Arc<PdsAuth>>,
    delegate_did: Option<String>,
) -> LuaResult<()> {
    // -- methods table (shared by all Record instances) --
    let methods = lua.create_table()?;

    // :save()
    {
        let state = state.clone();
        let claims = claims.clone();
        let pds_auth = pds_auth.clone();
        let delegate_did = delegate_did.clone();
        let save_fn = lua.create_async_function(move |lua, this: mlua::Table| {
            let state = state.clone();
            let claims = claims.clone();
            let pds_auth = pds_auth.clone();
            let delegate_did = delegate_did.clone();
            async move {
                let claims = claims.ok_or_else(|| mlua::Error::runtime(NO_PDS_AUTH_MSG))?;
                let pds_auth = pds_auth.ok_or_else(|| mlua::Error::runtime(NO_PDS_AUTH_MSG))?;
                let backend = state.db_backend;
                let collection: String = this.raw_get("_collection")?;
                let schema: mlua::Value = this.raw_get("_schema")?;
                let repo_override: Option<String> = this.raw_get("_repo_override")?;
                let repo = repo_override
                    .as_deref()
                    .or(delegate_did.as_deref())
                    .unwrap_or_else(|| claims.did());

                // Validate required fields against schema
                if let mlua::Value::Table(ref schema_table) = schema {
                    validate_required_fields(&this, schema_table)?;
                }

                // Serialize record data (skip _ keys, inject $type)
                let data = extract_record_data(&lua, &this, &collection)?;

                let existing_uri: Option<String> = this.raw_get("_uri")?;

                let pds_result = if let Some(ref uri) = existing_uri {
                    // PUT
                    let rkey = uri
                        .split('/')
                        .next_back()
                        .ok_or_else(|| mlua::Error::runtime("invalid AT URI"))?
                        .to_string();

                    let pds_body = json!({
                        "repo": repo,
                        "collection": collection,
                        "rkey": rkey,
                        "record": data,
                    });

                    let resp = pds_auth
                        .post_json(&state, repo, "com.atproto.repo.putRecord", &pds_body)
                        .await
                        .map_err(|e| mlua::Error::runtime(format!("PDS putRecord failed: {e}")))?;

                    if !resp.status().is_success() {
                        let status = resp.status();
                        let body = resp.text().await.unwrap_or_default();
                        return Err(mlua::Error::runtime(format!(
                            "PDS putRecord returned {status}: {body}"
                        )));
                    }

                    let bytes = resp.bytes().await.map_err(|e| {
                        mlua::Error::runtime(format!("failed to read PDS response: {e}"))
                    })?;
                    let result: Value = serde_json::from_slice(&bytes)
                        .map_err(|e| mlua::Error::runtime(format!("invalid PDS JSON: {e}")))?;

                    // Upsert local DB
                    let cid = result
                        .get("cid")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let now = now_rfc3339();
                    let data_str = serde_json::to_string(&data).unwrap_or_default();
                    let upsert_sql = adapt_sql(
                        r#"INSERT INTO records (uri, did, collection, rkey, record, cid, indexed_at, created_at)
                           VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                           ON CONFLICT (uri) DO UPDATE
                               SET record = EXCLUDED.record,
                                   cid = EXCLUDED.cid,
                                   indexed_at = ?"#,
                        backend,
                    );
                    let _ = sqlx::query(&upsert_sql)
                        .bind(uri)
                        .bind(repo)
                        .bind(&collection)
                        .bind(&rkey)
                        .bind(&data_str)
                        .bind(cid)
                        .bind(&now)
                        .bind(&now)
                        .bind(&now)
                        .execute(&state.db)
                        .await;

                    let _ = sync_refs(&state.db, uri, &collection, &data, backend).await;

                    result
                } else {
                    // CREATE
                    let rkey: Option<String> = this.raw_get("_rkey")?;
                    let mut pds_body = json!({
                        "repo": repo,
                        "collection": collection,
                        "record": data,
                    });
                    if let Some(ref rkey) = rkey {
                        pds_body["rkey"] = json!(rkey);
                    }

                    let resp = pds_auth
                        .post_json(&state, repo, "com.atproto.repo.createRecord", &pds_body)
                        .await
                        .map_err(|e| mlua::Error::runtime(format!("PDS createRecord failed: {e}")))?;

                    if !resp.status().is_success() {
                        let status = resp.status();
                        let body = resp.text().await.unwrap_or_default();
                        return Err(mlua::Error::runtime(format!(
                            "PDS createRecord returned {status}: {body}"
                        )));
                    }

                    let bytes = resp.bytes().await.map_err(|e| {
                        mlua::Error::runtime(format!("failed to read PDS response: {e}"))
                    })?;
                    let result: Value = serde_json::from_slice(&bytes)
                        .map_err(|e| mlua::Error::runtime(format!("invalid PDS JSON: {e}")))?;

                    // Upsert local DB
                    if let (Some(uri), Some(cid)) = (
                        result.get("uri").and_then(|v| v.as_str()),
                        result.get("cid").and_then(|v| v.as_str()),
                    ) {
                        let rkey = uri.split('/').next_back().unwrap_or_default();
                        let data_str = serde_json::to_string(&data).unwrap_or_default();
                        let now = now_rfc3339();
                        let upsert_sql = adapt_sql(
                            r#"INSERT INTO records (uri, did, collection, rkey, record, cid, created_at)
                               VALUES (?, ?, ?, ?, ?, ?, ?)
                               ON CONFLICT (uri) DO UPDATE
                                   SET record = EXCLUDED.record,
                                       cid = EXCLUDED.cid"#,
                            backend,
                        );
                        let _ = sqlx::query(&upsert_sql)
                            .bind(uri)
                            .bind(repo)
                            .bind(&collection)
                            .bind(rkey)
                            .bind(&data_str)
                            .bind(cid)
                            .bind(&now)
                            .execute(&state.db)
                            .await;

                        let _ = sync_refs(&state.db, uri, &collection, &data, backend).await;
                    }

                    result
                };

                // Write back _uri and _cid
                if let Some(uri) = pds_result.get("uri").and_then(|v| v.as_str()) {
                    this.raw_set("_uri", uri.to_string())?;
                }
                if let Some(cid) = pds_result.get("cid").and_then(|v| v.as_str()) {
                    this.raw_set("_cid", cid.to_string())?;
                }

                Ok(this)
            }
        })?;
        methods.set("save", save_fn)?;
    }

    // :delete()
    {
        let state = state.clone();
        let claims = claims.clone();
        let pds_auth = pds_auth.clone();
        let delegate_did = delegate_did.clone();
        let delete_fn = lua.create_async_function(move |_lua, this: mlua::Table| {
            let state = state.clone();
            let claims = claims.clone();
            let pds_auth = pds_auth.clone();
            let delegate_did = delegate_did.clone();
            async move {
                let claims = claims.ok_or_else(|| mlua::Error::runtime(NO_PDS_AUTH_MSG))?;
                let pds_auth = pds_auth.ok_or_else(|| mlua::Error::runtime(NO_PDS_AUTH_MSG))?;
                let backend = state.db_backend;
                let uri: String = this.raw_get::<Option<String>>("_uri")?.ok_or_else(|| {
                    mlua::Error::runtime("cannot delete a Record that has no _uri")
                })?;
                let collection: String = this.raw_get("_collection")?;
                let repo_override: Option<String> = this.raw_get("_repo_override")?;
                let repo = repo_override
                    .as_deref()
                    .or(delegate_did.as_deref())
                    .unwrap_or_else(|| claims.did());

                let rkey = uri
                    .split('/')
                    .next_back()
                    .ok_or_else(|| mlua::Error::runtime("invalid AT URI"))?
                    .to_string();

                let pds_body = json!({
                    "repo": repo,
                    "collection": collection,
                    "rkey": rkey,
                });

                // Try the PDS delete. We log-and-continue on failure so the
                // operator's intent ("remove this record") is still
                // reflected in the local DB even when the PDS is down or
                // refuses the call. The local row is the source of truth
                // for the index.
                match pds_auth
                    .post_json(&state, repo, "com.atproto.repo.deleteRecord", &pds_body)
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {}
                    Ok(resp) => {
                        let status = resp.status();
                        let body = resp.text().await.unwrap_or_default();
                        tracing::warn!(
                            uri = %uri,
                            "PDS deleteRecord returned {status}: {body} \
                             — proceeding with local delete anyway"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            uri = %uri,
                            "PDS deleteRecord failed: {e} \
                             — proceeding with local delete anyway"
                        );
                    }
                }

                // Always delete locally — operator's logical action is
                // "remove this record from view" regardless of PDS outcome.
                let delete_sql = adapt_sql("DELETE FROM records WHERE uri = ?", backend);
                let _ = sqlx::query(&delete_sql).bind(&uri).execute(&state.db).await;

                this.raw_set("_uri", mlua::Value::Nil)?;
                this.raw_set("_cid", mlua::Value::Nil)?;

                Ok(this)
            }
        })?;
        methods.set("delete", delete_fn)?;
    }

    // :save_local() — upsert into local DB only, never touches a PDS.
    // Works in any script context (no auth required).
    //
    // For records loaded via `Record.load(uri)` or saved via `:save()`
    // (i.e. those with an `_uri`), the repo+rkey are parsed back out of
    // the URI. For brand-new records (no `_uri`), we fall back to
    // `_repo_override` / `claims.did()` for the repo, and generate an
    // rkey via `_key_type` if `_rkey` isn't set. Errors clearly when no
    // DID can be determined.
    {
        let state = state.clone();
        let claims = claims.clone();
        let save_local_fn = lua.create_async_function(move |lua, this: mlua::Table| {
            let state = state.clone();
            let claims = claims.clone();
            async move {
                let backend = state.db_backend;
                let collection: String = this.raw_get("_collection")?;
                let schema: mlua::Value = this.raw_get("_schema")?;

                if let mlua::Value::Table(ref schema_table) = schema {
                    validate_required_fields(&this, schema_table)?;
                }

                let data = extract_record_data(&lua, &this, &collection)?;
                let data_str = serde_json::to_string(&data).unwrap_or_default();
                let now = now_rfc3339();

                let existing_uri: Option<String> = this.raw_get("_uri")?;
                let (uri, repo, rkey) = if let Some(uri) = existing_uri {
                    // Parse repo (DID) and rkey out of the URI:
                    // at://<did>/<collection>/<rkey>
                    let trimmed = uri
                        .strip_prefix("at://")
                        .ok_or_else(|| mlua::Error::runtime(format!("invalid AT URI: {uri}")))?;
                    let mut parts = trimmed.splitn(3, '/');
                    let repo = parts
                        .next()
                        .ok_or_else(|| mlua::Error::runtime(format!("invalid AT URI: {uri}")))?
                        .to_string();
                    let _col = parts.next();
                    let rkey = parts
                        .next()
                        .ok_or_else(|| mlua::Error::runtime(format!("invalid AT URI: {uri}")))?
                        .to_string();
                    (uri, repo, rkey)
                } else {
                    // CREATE path — no URI yet. Compute repo + rkey, build URI.
                    let repo_override: Option<String> = this.raw_get("_repo_override")?;
                    let repo = repo_override
                        .or_else(|| claims.as_ref().map(|c| c.did().to_string()))
                        .ok_or_else(|| {
                            mlua::Error::runtime(
                                "save_local() needs a DID — call :set_repo(\"did:plc:...\") \
                                 or load the record first",
                            )
                        })?;

                    let rkey: Option<String> = this.raw_get("_rkey")?;
                    let rkey = if let Some(rk) = rkey {
                        rk
                    } else {
                        let key_type: Option<String> = this.raw_get("_key_type")?;
                        match key_type.as_deref() {
                            Some("tid") | Some("any") | None => generate_tid(),
                            Some(s) if s.starts_with("literal:") => {
                                s["literal:".len()..].to_string()
                            }
                            Some("nsid") => {
                                return Err(mlua::Error::runtime(
                                    "cannot auto-generate rkey for nsid key type — \
                                     call set_rkey() first",
                                ));
                            }
                            Some(other) => {
                                return Err(mlua::Error::runtime(format!(
                                    "unknown key type '{other}'"
                                )));
                            }
                        }
                    };
                    let uri = format!("at://{repo}/{collection}/{rkey}");
                    (uri, repo, rkey)
                };

                // Upsert. Sentinel CID `""` — no PDS round-trip means we
                // have no real CID to record; consumers reading the row
                // should treat empty CID as "local-only write".
                let upsert_sql = adapt_sql(
                    r#"INSERT INTO records (uri, did, collection, rkey, record, cid, indexed_at, created_at)
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                       ON CONFLICT (uri) DO UPDATE
                           SET record = EXCLUDED.record,
                               cid = EXCLUDED.cid,
                               indexed_at = ?"#,
                    backend,
                );
                sqlx::query(&upsert_sql)
                    .bind(&uri)
                    .bind(&repo)
                    .bind(&collection)
                    .bind(&rkey)
                    .bind(&data_str)
                    .bind("")
                    .bind(&now)
                    .bind(&now)
                    .bind(&now)
                    .execute(&state.db)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("save_local upsert failed: {e}")))?;

                let _ = sync_refs(&state.db, &uri, &collection, &data, backend).await;

                this.raw_set("_uri", uri.as_str())?;
                this.raw_set("_cid", "")?;

                Ok(this)
            }
        })?;
        methods.set("save_local", save_local_fn)?;
    }

    // :delete_local() — local DB delete only, never touches a PDS.
    // Idempotent: succeeds whether or not a row existed at the URI.
    {
        let state = state.clone();
        let delete_local_fn = lua.create_async_function(move |_lua, this: mlua::Table| {
            let state = state.clone();
            async move {
                let backend = state.db_backend;
                let uri: String = this.raw_get::<Option<String>>("_uri")?.ok_or_else(|| {
                    mlua::Error::runtime("cannot delete_local a Record that has no _uri")
                })?;

                let delete_sql = adapt_sql("DELETE FROM records WHERE uri = ?", backend);
                sqlx::query(&delete_sql)
                    .bind(&uri)
                    .execute(&state.db)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("delete_local failed: {e}")))?;

                this.raw_set("_uri", mlua::Value::Nil)?;
                this.raw_set("_cid", mlua::Value::Nil)?;

                Ok(this)
            }
        })?;
        methods.set("delete_local", delete_local_fn)?;
    }

    // :set_key_type(type)
    {
        let set_key_type_fn =
            lua.create_function(|_lua, (this, key_type): (mlua::Table, String)| {
                match key_type.as_str() {
                    "tid" | "any" | "nsid" => {}
                    s if s.starts_with("literal:") && s.len() > "literal:".len() => {}
                    _ => {
                        return Err(mlua::Error::runtime(format!(
                            "invalid key type '{key_type}': expected tid, any, nsid, or literal:*"
                        )));
                    }
                }
                this.raw_set("_key_type", key_type)?;
                Ok(this)
            })?;
        methods.set("set_key_type", set_key_type_fn)?;
    }

    // :set_rkey(key)
    {
        let set_rkey_fn = lua.create_function(|_lua, (this, key): (mlua::Table, String)| {
            if key.is_empty() {
                return Err(mlua::Error::runtime("rkey must be a non-empty string"));
            }
            this.raw_set("_rkey", key)?;
            Ok(this)
        })?;
        methods.set("set_rkey", set_rkey_fn)?;
    }

    // :set_repo(did)
    {
        let set_repo_fn = lua.create_function(|_lua, (this, did): (mlua::Table, String)| {
            if did.is_empty() {
                return Err(mlua::Error::runtime("did must be a non-empty string"));
            }
            this.raw_set("_repo_override", did)?;
            Ok(this)
        })?;
        methods.set("set_repo", set_repo_fn)?;
    }

    // :generate_rkey()
    {
        let generate_rkey_fn = lua.create_function(|_lua, this: mlua::Table| {
            let key_type: Option<String> = this.raw_get("_key_type")?;
            let rkey = match key_type.as_deref() {
                Some("tid") | Some("any") => generate_tid(),
                Some(s) if s.starts_with("literal:") => s["literal:".len()..].to_string(),
                Some("nsid") => {
                    return Err(mlua::Error::runtime(
                        "cannot auto-generate rkey for nsid key type — use set_rkey() instead",
                    ));
                }
                Some(other) => {
                    return Err(mlua::Error::runtime(format!("unknown key type '{other}'")));
                }
                None => {
                    return Err(mlua::Error::runtime(
                        "no _key_type set — call set_key_type() first or use a record-type lexicon",
                    ));
                }
            };
            this.raw_set("_rkey", rkey.as_str())?;
            Ok(rkey)
        })?;
        methods.set("generate_rkey", generate_rkey_fn)?;
    }

    // -- metatable --
    let metatable = lua.create_table()?;

    // __index: check methods first, then rawget
    {
        let methods_ref = methods.clone();
        let index_fn = lua.create_function(move |_lua, (this, key): (mlua::Table, String)| {
            // Check methods table first
            let method: mlua::Value = methods_ref.raw_get(key.as_str())?;
            if !method.is_nil() {
                return Ok(method);
            }
            // Fall through to raw field access
            this.raw_get::<mlua::Value>(key.as_str())
        })?;
        metatable.set("__index", index_fn)?;
    }

    // __newindex: block writes to internal fields
    {
        let newindex_fn = lua.create_function(
            move |_lua, (this, key, value): (mlua::Table, String, mlua::Value)| {
                if INTERNAL_FIELDS.contains(&key.as_str()) {
                    return Err(mlua::Error::runtime(format!(
                        "cannot assign to internal field '{key}'"
                    )));
                }
                this.raw_set(key, value)?;
                Ok(())
            },
        )?;
        metatable.set("__newindex", newindex_fn)?;
    }

    // __tostring
    {
        let tostring_fn = lua.create_function(|_lua, this: mlua::Table| {
            let collection: String = this.raw_get("_collection")?;
            let uri: Option<String> = this.raw_get("_uri")?;
            match uri {
                Some(u) => Ok(format!("Record({collection}) [uri={u}]")),
                None => Ok(format!("Record({collection}) [unsaved]")),
            }
        })?;
        metatable.set("__tostring", tostring_fn)?;
    }

    // -- Record constructor function --
    let record_table = lua.create_table()?;

    {
        let state_c = state.clone();
        let metatable_c = metatable.clone();
        let constructor = lua.create_async_function(
            move |lua, (collection, data): (String, Option<mlua::Value>)| {
                let state = state_c.clone();
                let metatable = metatable_c.clone();
                async move {
                    let table = lua.create_table()?;

                    // Look up schema
                    let lexicon = state.lexicons.get(&collection).await;
                    let schema_value: mlua::Value =
                        match lexicon.as_ref().and_then(|l| l.record_schema.as_ref()) {
                            Some(schema_json) => lua.to_value(schema_json)?,
                            None => mlua::Value::Nil,
                        };

                    // Set internal fields
                    table.raw_set("_collection", collection.as_str())?;
                    table.raw_set("_uri", mlua::Value::Nil)?;
                    table.raw_set("_cid", mlua::Value::Nil)?;
                    table.raw_set("_schema", schema_value.clone())?;
                    table.raw_set("_repo_override", mlua::Value::Nil)?;

                    // Auto-set _key_type from the lexicon's record_key
                    match lexicon.as_ref().and_then(|l| l.record_key.as_deref()) {
                        Some(key) => table.raw_set("_key_type", key)?,
                        None => table.raw_set("_key_type", mlua::Value::Nil)?,
                    }
                    table.raw_set("_rkey", mlua::Value::Nil)?;

                    // Copy fields from data if provided
                    if let Some(mlua::Value::Table(data_table)) = data {
                        for pair in data_table.pairs::<mlua::Value, mlua::Value>() {
                            let (k, v) = pair?;
                            table.raw_set(k, v)?;
                        }
                    }

                    // Populate defaults from schema
                    if let mlua::Value::Table(ref schema_table) = schema_value {
                        populate_defaults(&lua, &table, schema_table)?;
                    }

                    table.set_metatable(Some(metatable))?;
                    Ok(table)
                }
            },
        )?;
        record_table.set("new", constructor)?;
    }

    // -- Static methods --

    // Record.save_all(records)
    {
        let state = state.clone();
        let claims = claims.clone();
        let pds_auth = pds_auth.clone();
        let delegate_did = delegate_did.clone();
        let save_all_fn =
            lua.create_async_function(move |lua, records_table: mlua::Table| {
                let state = state.clone();
                let claims = claims.clone();
                let pds_auth = pds_auth.clone();
                let delegate_did = delegate_did.clone();
                async move {
                    let claims = claims.ok_or_else(|| mlua::Error::runtime(NO_PDS_AUTH_MSG))?;
                    let pds_auth = pds_auth.ok_or_else(|| mlua::Error::runtime(NO_PDS_AUTH_MSG))?;
                    let backend = state.db_backend;
                    // Extract save data from each record (sync)
                    type SaveItem = (mlua::Table, String, Option<String>, Option<String>, Option<String>, Value);
                    let mut save_items: Vec<SaveItem> = Vec::new();

                    for pair in records_table.sequence_values::<mlua::Table>() {
                        let record_table = pair?;
                        let collection: String = record_table.raw_get("_collection")?;
                        let existing_uri: Option<String> = record_table.raw_get("_uri")?;
                        let rkey: Option<String> = record_table.raw_get("_rkey")?;
                        let repo_override: Option<String> = record_table.raw_get("_repo_override")?;

                        // Validate
                        let schema: mlua::Value = record_table.raw_get("_schema")?;
                        if let mlua::Value::Table(ref schema_table) = schema {
                            validate_required_fields(&record_table, schema_table)?;
                        }

                        let data = extract_record_data(&lua, &record_table, &collection)?;
                        save_items.push((record_table, collection, existing_uri, rkey, repo_override, data));
                    }

                    // Parallel PDS calls
                    let futs = save_items.iter().map(|(_, collection, existing_uri, rkey, repo_override, data)| {
                        let state = state.clone();
                        let claims = claims.clone();
                        let pds_auth = pds_auth.clone();
                        let delegate_did = delegate_did.clone();
                        let collection = collection.clone();
                        let existing_uri = existing_uri.clone();
                        let rkey = rkey.clone();
                        let repo_override = repo_override.clone();
                        let data = data.clone();
                        async move {
                            let repo = repo_override
                                .as_deref()
                                .or(delegate_did.as_deref())
                                .unwrap_or_else(|| claims.did());
                            if let Some(ref uri) = existing_uri {
                                let rkey = uri
                                    .split('/')
                                    .next_back()
                                    .ok_or_else(|| mlua::Error::runtime("invalid AT URI"))?
                                    .to_string();

                                let pds_body = json!({
                                    "repo": repo,
                                    "collection": collection,
                                    "rkey": rkey,
                                    "record": data,
                                });

                                let resp = pds_auth
                                    .post_json(&state, repo, "com.atproto.repo.putRecord", &pds_body)
                                    .await
                                    .map_err(|e| {
                                        mlua::Error::runtime(format!("PDS putRecord failed: {e}"))
                                    })?;

                                if !resp.status().is_success() {
                                    let status = resp.status();
                                    let body = resp.text().await.unwrap_or_default();
                                    return Err(mlua::Error::runtime(format!(
                                        "PDS putRecord returned {status}: {body}"
                                    )));
                                }

                                let bytes = resp.bytes().await.map_err(|e| {
                                    mlua::Error::runtime(format!(
                                        "failed to read PDS response: {e}"
                                    ))
                                })?;
                                let result: Value = serde_json::from_slice(&bytes).map_err(
                                    |e| mlua::Error::runtime(format!("invalid PDS JSON: {e}")),
                                )?;

                                let cid = result
                                    .get("cid")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();
                                let now = now_rfc3339();
                                let data_str = serde_json::to_string(&data).unwrap_or_default();
                                let upsert_sql = adapt_sql(
                                    r#"INSERT INTO records (uri, did, collection, rkey, record, cid, indexed_at, created_at)
                                       VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                                       ON CONFLICT (uri) DO UPDATE
                                           SET record = EXCLUDED.record,
                                               cid = EXCLUDED.cid,
                                               indexed_at = ?"#,
                                    backend,
                                );
                                let _ = sqlx::query(&upsert_sql)
                                .bind(uri.as_str())
                                .bind(repo)
                                .bind(&collection)
                                .bind(&rkey)
                                .bind(&data_str)
                                .bind(cid)
                                .bind(&now)
                                .bind(&now)
                                .bind(&now)
                                .execute(&state.db)
                                .await;

                                let _ = sync_refs(&state.db, uri.as_str(), &collection, &data, backend).await;

                                Ok(result)
                            } else {
                                let mut pds_body = json!({
                                    "repo": repo,
                                    "collection": collection,
                                    "record": data,
                                });
                                if let Some(ref rkey) = rkey {
                                    pds_body["rkey"] = json!(rkey);
                                }

                                let resp = pds_auth
                                    .post_json(&state, repo, "com.atproto.repo.createRecord", &pds_body)
                                    .await
                                    .map_err(|e| {
                                        mlua::Error::runtime(format!(
                                            "PDS createRecord failed: {e}"
                                        ))
                                    })?;

                                if !resp.status().is_success() {
                                    let status = resp.status();
                                    let body = resp.text().await.unwrap_or_default();
                                    return Err(mlua::Error::runtime(format!(
                                        "PDS createRecord returned {status}: {body}"
                                    )));
                                }

                                let bytes = resp.bytes().await.map_err(|e| {
                                    mlua::Error::runtime(format!(
                                        "failed to read PDS response: {e}"
                                    ))
                                })?;
                                let result: Value = serde_json::from_slice(&bytes).map_err(
                                    |e| mlua::Error::runtime(format!("invalid PDS JSON: {e}")),
                                )?;

                                if let (Some(uri), Some(cid)) = (
                                    result.get("uri").and_then(|v| v.as_str()),
                                    result.get("cid").and_then(|v| v.as_str()),
                                ) {
                                    let rkey =
                                        uri.split('/').next_back().unwrap_or_default();
                                    let data_str = serde_json::to_string(&data).unwrap_or_default();
                                    let now = now_rfc3339();
                                    let upsert_sql = adapt_sql(
                                        r#"INSERT INTO records (uri, did, collection, rkey, record, cid, created_at)
                                           VALUES (?, ?, ?, ?, ?, ?, ?)
                                           ON CONFLICT (uri) DO UPDATE
                                               SET record = EXCLUDED.record,
                                                   cid = EXCLUDED.cid"#,
                                        backend,
                                    );
                                    let _ = sqlx::query(&upsert_sql)
                                    .bind(uri)
                                    .bind(repo)
                                    .bind(&collection)
                                    .bind(rkey)
                                    .bind(&data_str)
                                    .bind(cid)
                                    .bind(&now)
                                    .execute(&state.db)
                                    .await;

                                    let _ = sync_refs(&state.db, uri, &collection, &data, backend).await;
                                }

                                Ok(result)
                            }
                        }
                    });

                    let results = try_join_all(futs).await?;

                    // Write back _uri and _cid (sync)
                    for (i, (record_table, _, _, _, _, _)) in save_items.iter().enumerate() {
                        if let Some(result) = results.get(i) {
                            if let Some(uri) = result.get("uri").and_then(|v| v.as_str()) {
                                record_table.raw_set("_uri", uri.to_string())?;
                            }
                            if let Some(cid) = result.get("cid").and_then(|v| v.as_str()) {
                                record_table.raw_set("_cid", cid.to_string())?;
                            }
                        }
                    }

                    lua.to_value(&results)
                }
            })?;
        record_table.set("save_all", save_all_fn)?;
    }

    // Record.load(uri)
    {
        let state = state.clone();
        let metatable_c = metatable.clone();
        let load_fn = lua.create_async_function(move |lua, uri: String| {
            let state = state.clone();
            let metatable = metatable_c.clone();
            async move {
                let backend = state.db_backend;
                let sql = adapt_sql(
                    "SELECT collection, record, cid FROM records WHERE uri = ?",
                    backend,
                );
                let row: Option<(String, String, String)> = sqlx::query_as(&sql)
                    .bind(&uri)
                    .fetch_optional(&state.db)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("DB query failed: {e}")))?;

                match row {
                    Some((collection, record_str, cid)) => {
                        let record: Value = serde_json::from_str(&record_str).unwrap_or(json!({}));
                        let table = lua.create_table()?;

                        // Look up schema
                        let lexicon = state.lexicons.get(&collection).await;
                        let schema_value: mlua::Value =
                            match lexicon.as_ref().and_then(|l| l.record_schema.as_ref()) {
                                Some(schema_json) => lua.to_value(schema_json)?,
                                None => mlua::Value::Nil,
                            };

                        table.raw_set("_collection", collection.as_str())?;
                        table.raw_set("_uri", uri.as_str())?;
                        table.raw_set("_cid", cid.as_str())?;
                        table.raw_set("_schema", schema_value)?;
                        table.raw_set("_key_type", mlua::Value::Nil)?;
                        table.raw_set("_rkey", mlua::Value::Nil)?;
                        table.raw_set("_repo_override", mlua::Value::Nil)?;

                        // Copy record fields
                        if let Some(obj) = record.as_object() {
                            for (k, v) in obj {
                                if k == "$type" {
                                    continue;
                                }
                                let lua_val: mlua::Value = lua.to_value(v)?;
                                table.raw_set(k.as_str(), lua_val)?;
                            }
                        }

                        table.set_metatable(Some(metatable))?;
                        Ok(mlua::Value::Table(table))
                    }
                    None => Ok(mlua::Value::Nil),
                }
            }
        })?;
        record_table.set("load", load_fn)?;
    }

    // Record.load_all(uris)
    {
        let state = state.clone();
        let metatable_c = metatable.clone();
        let load_all_fn = lua.create_async_function(move |lua, uris_table: mlua::Table| {
            let state = state.clone();
            let metatable = metatable_c.clone();
            async move {
                let backend = state.db_backend;
                let uris: Vec<String> = lua.from_value(mlua::Value::Table(uris_table))?;

                let futs = uris.iter().map(|uri| {
                    let state = state.clone();
                    let uri = uri.clone();
                    async move {
                        let sql = adapt_sql(
                            "SELECT collection, record, cid FROM records WHERE uri = ?",
                            backend,
                        );
                        let row: Option<(String, String, String)> = sqlx::query_as(&sql)
                            .bind(&uri)
                            .fetch_optional(&state.db)
                            .await
                            .map_err(|e| mlua::Error::runtime(format!("DB query failed: {e}")))?;

                        let result: Result<_, mlua::Error> =
                            Ok(row.map(|(collection, record_str, cid)| {
                                let record: Value =
                                    serde_json::from_str(&record_str).unwrap_or(json!({}));
                                (uri, collection, record, cid)
                            }));
                        result
                    }
                });

                let results: Vec<Option<(String, String, Value, String)>> =
                    try_join_all(futs).await?;

                let out = lua.create_table()?;
                for (i, item) in results.into_iter().enumerate() {
                    match item {
                        Some((uri, collection, record, cid)) => {
                            let table = lua.create_table()?;

                            let lexicon = state.lexicons.get(&collection).await;
                            let schema_value: mlua::Value =
                                match lexicon.as_ref().and_then(|l| l.record_schema.as_ref()) {
                                    Some(schema_json) => lua.to_value(schema_json)?,
                                    None => mlua::Value::Nil,
                                };

                            table.raw_set("_collection", collection.as_str())?;
                            table.raw_set("_uri", uri.as_str())?;
                            table.raw_set("_cid", cid.as_str())?;
                            table.raw_set("_schema", schema_value)?;
                            table.raw_set("_key_type", mlua::Value::Nil)?;
                            table.raw_set("_rkey", mlua::Value::Nil)?;
                            table.raw_set("_repo_override", mlua::Value::Nil)?;

                            if let Some(obj) = record.as_object() {
                                for (k, v) in obj {
                                    if k == "$type" {
                                        continue;
                                    }
                                    let lua_val: mlua::Value = lua.to_value(v)?;
                                    table.raw_set(k.as_str(), lua_val)?;
                                }
                            }

                            table.set_metatable(Some(metatable.clone()))?;
                            out.raw_set(i + 1, table)?;
                        }
                        None => {
                            out.raw_set(i + 1, mlua::Value::Nil)?;
                        }
                    }
                }

                Ok(mlua::Value::Table(out))
            }
        })?;
        record_table.set("load_all", load_all_fn)?;
    }

    // Record.delete_local(uri) — fire-and-forget local-only delete by URI.
    // The common one-liner for label-script reactions like:
    //   if event.val == "spam" then Record.delete_local(event.uri) end
    // Returns true if a row was deleted, false if no row matched.
    // Always succeeds (no error) regardless of whether the row existed.
    {
        let state = state;
        let delete_local_static_fn = lua.create_async_function(move |_lua, uri: String| {
            let state = state.clone();
            async move {
                let backend = state.db_backend;
                let delete_sql = adapt_sql("DELETE FROM records WHERE uri = ?", backend);
                let res = sqlx::query(&delete_sql)
                    .bind(&uri)
                    .execute(&state.db)
                    .await
                    .map_err(|e| {
                        mlua::Error::runtime(format!("Record.delete_local failed: {e}"))
                    })?;
                Ok(res.rows_affected() > 0)
            }
        })?;
        record_table.set("delete_local", delete_local_static_fn)?;
    }

    // -- Make Record callable via __call metamethod --
    let record_mt = lua.create_table()?;
    {
        let new_fn: mlua::Function = record_table.get("new")?;
        let call_fn =
            lua.create_async_function(
                move |_lua,
                      (_self_table, collection, data): (
                    mlua::Table,
                    String,
                    Option<mlua::Value>,
                )| {
                    let new_fn = new_fn.clone();
                    async move {
                        let result: mlua::Table = new_fn.call_async((collection, data)).await?;
                        Ok(result)
                    }
                },
            )?;
        record_mt.set("__call", call_fn)?;
    }
    record_table.set_metatable(Some(record_mt))?;

    lua.globals().set("Record", record_table)?;
    Ok(())
}

/// Check that all required fields (per schema) are present and non-nil.
fn validate_required_fields(table: &mlua::Table, schema: &mlua::Table) -> LuaResult<()> {
    let required: Option<mlua::Table> = schema.raw_get("required")?;
    if let Some(required) = required {
        for pair in required.sequence_values::<String>() {
            let field = pair?;
            let val: mlua::Value = table.raw_get(field.as_str())?;
            if val.is_nil() {
                return Err(mlua::Error::runtime(format!(
                    "missing required field '{field}'"
                )));
            }
        }
    }
    Ok(())
}

/// Set missing fields from schema property defaults.
fn populate_defaults(lua: &Lua, table: &mlua::Table, schema: &mlua::Table) -> LuaResult<()> {
    let properties: Option<mlua::Table> = schema.raw_get("properties")?;
    if let Some(properties) = properties {
        for pair in properties.pairs::<String, mlua::Table>() {
            let (key, prop_def) = pair?;
            // Skip internal fields
            if key.starts_with('_') {
                continue;
            }
            let existing: mlua::Value = table.raw_get(key.as_str())?;
            if existing.is_nil() {
                let default: mlua::Value = prop_def.raw_get("default")?;
                if !default.is_nil() {
                    table.raw_set(key.as_str(), lua.to_value(&default)?)?;
                }
            }
        }
    }
    Ok(())
}

/// Serialize a Record table to serde_json::Value, stripping _-prefixed keys,
/// filtering to only schema-defined properties, and injecting `$type`.
fn extract_record_data(lua: &Lua, table: &mlua::Table, collection: &str) -> LuaResult<Value> {
    // Build the set of allowed property names from the schema (if available).
    // When a schema is present, only fields listed in `properties` are included.
    let schema: mlua::Value = table.raw_get("_schema")?;
    let allowed: Option<Vec<String>> = if let mlua::Value::Table(ref schema_table) = schema {
        let properties: Option<mlua::Table> = schema_table.raw_get("properties")?;
        properties.map(|props| {
            props
                .pairs::<String, mlua::Value>()
                .filter_map(|pair| pair.ok().map(|(k, _)| k))
                .collect()
        })
    } else {
        None
    };

    let tmp = lua.create_table()?;
    for pair in table.pairs::<String, mlua::Value>() {
        let (k, v) = pair?;
        if k.starts_with('_') {
            continue;
        }
        if let Some(ref keys) = allowed
            && !keys.iter().any(|a| a == &k)
        {
            continue;
        }
        tmp.raw_set(k, v)?;
    }

    let mut data: Value = lua.from_value(mlua::Value::Table(tmp))?;
    if let Some(obj) = data.as_object_mut() {
        obj.insert("$type".to_string(), json!(collection));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn validate_required_fields_passes_when_present() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.raw_set("name", "test").unwrap();
        table.raw_set("count", 1).unwrap();

        let schema = lua.create_table().unwrap();
        let required = lua.create_sequence_from(["name", "count"]).unwrap();
        schema.raw_set("required", required).unwrap();

        assert!(validate_required_fields(&table, &schema).is_ok());
    }

    #[test]
    fn validate_required_fields_fails_when_missing() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.raw_set("name", "test").unwrap();

        let schema = lua.create_table().unwrap();
        let required = lua.create_sequence_from(["name", "count"]).unwrap();
        schema.raw_set("required", required).unwrap();

        let result = validate_required_fields(&table, &schema);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("count"),
            "expected error about 'count', got: {err}"
        );
    }

    #[test]
    fn populate_defaults_fills_missing_fields() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();

        let schema = lua.create_table().unwrap();
        let properties = lua.create_table().unwrap();
        let status_prop = lua.create_table().unwrap();
        status_prop.raw_set("default", "draft").unwrap();
        properties.raw_set("status", status_prop).unwrap();
        schema.raw_set("properties", properties).unwrap();

        populate_defaults(&lua, &table, &schema).unwrap();
        assert_eq!(table.raw_get::<String>("status").unwrap(), "draft");
    }

    #[test]
    fn populate_defaults_does_not_overwrite_existing() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.raw_set("status", "published").unwrap();

        let schema = lua.create_table().unwrap();
        let properties = lua.create_table().unwrap();
        let status_prop = lua.create_table().unwrap();
        status_prop.raw_set("default", "draft").unwrap();
        properties.raw_set("status", status_prop).unwrap();
        schema.raw_set("properties", properties).unwrap();

        populate_defaults(&lua, &table, &schema).unwrap();
        assert_eq!(table.raw_get::<String>("status").unwrap(), "published");
    }

    #[test]
    fn extract_record_data_strips_internal_fields() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.raw_set("_collection", "col").unwrap();
        table.raw_set("_uri", "at://did:plc:test/col/rkey").unwrap();
        table.raw_set("_schema", mlua::Value::Nil).unwrap();
        table.raw_set("name", "test").unwrap();

        let data = extract_record_data(&lua, &table, "com.example.thing").unwrap();
        let obj = data.as_object().unwrap();
        assert!(obj.contains_key("name"));
        assert!(obj.contains_key("$type"));
        assert!(!obj.contains_key("_collection"));
        assert!(!obj.contains_key("_uri"));
    }

    #[test]
    fn extract_record_data_filters_to_schema_properties() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.raw_set("name", "test").unwrap();
        table.raw_set("extra", "junk").unwrap();

        // Build a schema with only "name" in properties
        let schema = lua.create_table().unwrap();
        let properties = lua.create_table().unwrap();
        let name_prop = lua.create_table().unwrap();
        properties.raw_set("name", name_prop).unwrap();
        schema.raw_set("properties", properties).unwrap();
        table.raw_set("_schema", schema).unwrap();

        let data = extract_record_data(&lua, &table, "com.example.thing").unwrap();
        let obj = data.as_object().unwrap();
        assert!(obj.contains_key("name"));
        assert!(!obj.contains_key("extra"));
    }

    #[test]
    fn extract_record_data_injects_dollar_type() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.raw_set("_schema", mlua::Value::Nil).unwrap();
        table.raw_set("name", "test").unwrap();

        let data = extract_record_data(&lua, &table, "com.example.thing").unwrap();
        assert_eq!(data["$type"], "com.example.thing");
    }
}
