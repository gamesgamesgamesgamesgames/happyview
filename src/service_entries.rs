use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql};
use crate::error::AppError;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ServiceEntry {
    pub id: i64,
    pub fragment_id: String,
    pub service_type: String,
    pub access_mode: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateServiceEntry {
    pub fragment_id: String,
    pub service_type: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateServiceEntry {
    pub fragment_id: Option<String>,
    pub service_type: Option<String>,
    pub access_mode: Option<String>,
}

// ---------------------------------------------------------------------------
// Row type for service_entries
// ---------------------------------------------------------------------------

type ServiceEntryRow = (i64, String, String, String, String, String);

fn parse_service_entry_row(r: ServiceEntryRow) -> ServiceEntry {
    ServiceEntry {
        id: r.0,
        fragment_id: r.1,
        service_type: r.2,
        access_mode: r.3,
        created_at: r.4,
        updated_at: r.5,
    }
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

/// SELECT all service entries ordered by id.
pub async fn list_entries(
    db: &AnyPool,
    backend: DatabaseBackend,
) -> Result<Vec<ServiceEntry>, AppError> {
    let sql = adapt_sql(
        "SELECT id, fragment_id, service_type, access_mode, created_at, updated_at FROM happyview_service_entries ORDER BY id",
        backend,
    );

    let rows: Vec<ServiceEntryRow> = sqlx::query_as(&sql)
        .fetch_all(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list service entries: {e}")))?;

    Ok(rows.into_iter().map(parse_service_entry_row).collect())
}

/// INSERT a new service entry with access_mode='all', then return the created row.
pub async fn create_entry(
    db: &AnyPool,
    backend: DatabaseBackend,
    body: &CreateServiceEntry,
) -> Result<ServiceEntry, AppError> {
    let now = Utc::now().to_rfc3339();

    let insert_sql = adapt_sql(
        "INSERT INTO happyview_service_entries (fragment_id, service_type, access_mode, created_at, updated_at) VALUES (?, ?, 'all', ?, ?) RETURNING id",
        backend,
    );

    let row: (i64,) = sqlx::query_as(&insert_sql)
        .bind(&body.fragment_id)
        .bind(&body.service_type)
        .bind(&now)
        .bind(&now)
        .fetch_one(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create service entry: {e}")))?;

    let id = row.0;

    let fetch_sql = adapt_sql(
        "SELECT id, fragment_id, service_type, access_mode, created_at, updated_at FROM happyview_service_entries WHERE id = ?",
        backend,
    );

    let entry_row: ServiceEntryRow = sqlx::query_as(&fetch_sql)
        .bind(id)
        .fetch_one(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch created service entry: {e}")))?;

    Ok(parse_service_entry_row(entry_row))
}

/// Dynamic UPDATE — only provided fields are changed.
pub async fn update_entry(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: i64,
    body: &UpdateServiceEntry,
) -> Result<ServiceEntry, AppError> {
    if let Some(mode) = &body.access_mode
        && mode != "all"
        && mode != "specific"
    {
        return Err(AppError::BadRequest(format!(
            "invalid access_mode '{mode}': must be 'all' or 'specific'"
        )));
    }

    let now = Utc::now().to_rfc3339();

    let mut set_clauses: Vec<&str> = Vec::new();
    if body.fragment_id.is_some() {
        set_clauses.push("fragment_id = ?");
    }
    if body.service_type.is_some() {
        set_clauses.push("service_type = ?");
    }
    if body.access_mode.is_some() {
        set_clauses.push("access_mode = ?");
    }
    set_clauses.push("updated_at = ?");

    if set_clauses.len() == 1 {
        // Only updated_at — nothing meaningful to update; just fetch current state.
        let fetch_sql = adapt_sql(
            "SELECT id, fragment_id, service_type, access_mode, created_at, updated_at FROM happyview_service_entries WHERE id = ?",
            backend,
        );
        let row: Option<ServiceEntryRow> = sqlx::query_as(&fetch_sql)
            .bind(id)
            .fetch_optional(db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to fetch service entry: {e}")))?;
        return row
            .map(parse_service_entry_row)
            .ok_or_else(|| AppError::NotFound(format!("service entry {id} not found")));
    }

    let raw = format!(
        "UPDATE happyview_service_entries SET {} WHERE id = ?",
        set_clauses.join(", ")
    );
    let update_sql = adapt_sql(&raw, backend);

    let mut query = sqlx::query(&update_sql);
    if let Some(v) = &body.fragment_id {
        query = query.bind(v.as_str());
    }
    if let Some(v) = &body.service_type {
        query = query.bind(v.as_str());
    }
    if let Some(v) = &body.access_mode {
        query = query.bind(v.as_str());
    }
    query = query.bind(&now).bind(id);

    let result = query
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update service entry: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("service entry {id} not found")));
    }

    let fetch_sql = adapt_sql(
        "SELECT id, fragment_id, service_type, access_mode, created_at, updated_at FROM happyview_service_entries WHERE id = ?",
        backend,
    );
    let row: ServiceEntryRow = sqlx::query_as(&fetch_sql)
        .bind(id)
        .fetch_one(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch updated service entry: {e}")))?;

    Ok(parse_service_entry_row(row))
}

/// DELETE a service entry by id.
pub async fn delete_entry(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: i64,
) -> Result<bool, AppError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_service_entries WHERE id = ?",
        backend,
    );

    let result = sqlx::query(&sql)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete service entry: {e}")))?;

    Ok(result.rows_affected() > 0)
}

// ---------------------------------------------------------------------------
// Junction table: service_entry_xrpcs
// ---------------------------------------------------------------------------

/// SELECT lexicon_ids associated with a service entry.
pub async fn list_entry_xrpcs(
    db: &AnyPool,
    backend: DatabaseBackend,
    entry_id: i64,
) -> Result<Vec<String>, AppError> {
    let sql = adapt_sql(
        "SELECT lexicon_id FROM happyview_service_entry_xrpcs WHERE service_entry_id = ? ORDER BY lexicon_id",
        backend,
    );

    let rows: Vec<(String,)> = sqlx::query_as(&sql)
        .bind(entry_id)
        .fetch_all(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list entry xrpcs: {e}")))?;

    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// INSERT each lexicon_id for the entry with ON CONFLICT DO NOTHING.
pub async fn add_entry_xrpcs(
    db: &AnyPool,
    backend: DatabaseBackend,
    entry_id: i64,
    lexicon_ids: &[String],
) -> Result<(), AppError> {
    let sql = adapt_sql(
        "INSERT INTO happyview_service_entry_xrpcs (service_entry_id, lexicon_id) VALUES (?, ?) ON CONFLICT DO NOTHING",
        backend,
    );

    for lexicon_id in lexicon_ids {
        sqlx::query(&sql)
            .bind(entry_id)
            .bind(lexicon_id.as_str())
            .execute(db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to add entry xrpc: {e}")))?;
    }

    Ok(())
}

/// DELETE each lexicon_id association for the entry.
pub async fn remove_entry_xrpcs(
    db: &AnyPool,
    backend: DatabaseBackend,
    entry_id: i64,
    lexicon_ids: &[String],
) -> Result<(), AppError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_service_entry_xrpcs WHERE service_entry_id = ? AND lexicon_id = ?",
        backend,
    );

    for lexicon_id in lexicon_ids {
        sqlx::query(&sql)
            .bind(entry_id)
            .bind(lexicon_id.as_str())
            .execute(db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to remove entry xrpc: {e}")))?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Access checks
// ---------------------------------------------------------------------------

/// Return true if the fragment/xrpc combination is accessible.
///
/// - access_mode = 'all'      → always true
/// - access_mode = 'specific' → true only if xrpc_method is in the junction table
pub async fn check_access(
    db: &AnyPool,
    backend: DatabaseBackend,
    fragment_id: &str,
    xrpc_method: &str,
) -> Result<bool, AppError> {
    let sql = adapt_sql(
        "SELECT id, access_mode FROM happyview_service_entries WHERE fragment_id = ? LIMIT 1",
        backend,
    );

    let row: Option<(i64, String)> = sqlx::query_as(&sql)
        .bind(fragment_id)
        .fetch_optional(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to check service entry access: {e}")))?;

    let (entry_id, access_mode) = match row {
        None => return Ok(false),
        Some(r) => r,
    };

    if access_mode == "all" {
        return Ok(true);
    }

    // access_mode = "specific" — check junction table
    let check_sql = adapt_sql(
        "SELECT 1 FROM happyview_service_entry_xrpcs WHERE service_entry_id = ? AND lexicon_id = ? LIMIT 1",
        backend,
    );

    let found: Option<(i32,)> = sqlx::query_as(&check_sql)
        .bind(entry_id)
        .bind(xrpc_method)
        .fetch_optional(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to check xrpc access: {e}")))?;

    Ok(found.is_some())
}

/// Return all service entries that grant access to a given lexicon.
///
/// Includes entries where access_mode='all' or where the lexicon_id is in the junction table.
pub async fn services_for_lexicon(
    db: &AnyPool,
    backend: DatabaseBackend,
    lexicon_id: &str,
) -> Result<Vec<ServiceEntry>, AppError> {
    let sql = adapt_sql(
        "SELECT id, fragment_id, service_type, access_mode, created_at, updated_at FROM happyview_service_entries WHERE access_mode = 'all' OR EXISTS (SELECT 1 FROM happyview_service_entry_xrpcs WHERE happyview_service_entry_xrpcs.service_entry_id = happyview_service_entries.id AND happyview_service_entry_xrpcs.lexicon_id = ?) ORDER BY id",
        backend,
    );

    let rows: Vec<ServiceEntryRow> = sqlx::query_as(&sql)
        .bind(lexicon_id)
        .fetch_all(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query services for lexicon: {e}")))?;

    Ok(rows.into_iter().map(parse_service_entry_row).collect())
}
