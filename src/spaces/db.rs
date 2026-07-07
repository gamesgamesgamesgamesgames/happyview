use crate::db::{DatabaseBackend, adapt_sql, decode_cursor, encode_cursor, now_rfc3339};
use crate::error::AppError;
use crate::spaces::types::*;

// ---------------------------------------------------------------------------
// Spaces
// ---------------------------------------------------------------------------

pub async fn create_space(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space: &Space,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let config_json = serde_json::to_string(&space.config)
        .map_err(|e| AppError::Internal(format!("failed to serialize space config: {e}")))?;
    let app_access_json = serde_json::to_string(&space.app_access)
        .map_err(|e| AppError::Internal(format!("failed to serialize app_access: {e}")))?;

    let sql = adapt_sql(
        "INSERT INTO happyview_spaces (id, did, authority_did, creator_did, type_nsid, skey, display_name, description, mint_policy, app_access, managing_app_did, config, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        backend,
    );

    sqlx::query(&sql)
        .bind(&space.id)
        .bind(&space.did)
        .bind(&space.authority_did)
        .bind(&space.creator_did)
        .bind(&space.type_nsid)
        .bind(&space.skey)
        .bind(&space.display_name)
        .bind(&space.description)
        .bind(space.mint_policy.as_str())
        .bind(&app_access_json)
        .bind(&space.managing_app_did)
        .bind(&config_json)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create space: {e}")))?;

    Ok(())
}

pub async fn get_space(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<Option<Space>, AppError> {
    let sql = adapt_sql(
        "SELECT id, did, authority_did, creator_did, type_nsid, skey, display_name, description, mint_policy, app_access, managing_app_did, config, revision, created_at, updated_at FROM happyview_spaces WHERE id = ?",
        backend,
    );

    let row: Option<SpaceRow> = sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get space: {e}")))?;

    row.map(parse_space_row).transpose()
}

pub async fn get_space_by_address(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    did: &str,
    type_nsid: &str,
    skey: &str,
) -> Result<Option<Space>, AppError> {
    let sql = adapt_sql(
        "SELECT id, did, authority_did, creator_did, type_nsid, skey, display_name, description, mint_policy, app_access, managing_app_did, config, revision, created_at, updated_at FROM happyview_spaces WHERE did = ? AND type_nsid = ? AND skey = ?",
        backend,
    );

    let row: Option<SpaceRow> = sqlx::query_as(&sql)
        .bind(did)
        .bind(type_nsid)
        .bind(skey)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get space: {e}")))?;

    row.map(parse_space_row).transpose()
}

pub async fn list_spaces_by_owner(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    authority_did: &str,
) -> Result<Vec<Space>, AppError> {
    let sql = adapt_sql(
        "SELECT id, did, authority_did, creator_did, type_nsid, skey, display_name, description, mint_policy, app_access, managing_app_did, config, revision, created_at, updated_at FROM happyview_spaces WHERE authority_did = ? ORDER BY created_at DESC",
        backend,
    );

    let rows: Vec<SpaceRow> = sqlx::query_as(&sql)
        .bind(authority_did)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list spaces: {e}")))?;

    rows.into_iter().map(parse_space_row).collect()
}

pub struct SpaceView {
    pub uri: String,
    pub is_owner: bool,
    pub created_at: String,
}

pub async fn list_spaces_for_user(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    did: &str,
    limit: i64,
    cursor: Option<&str>,
) -> Result<(Vec<SpaceView>, Option<String>), AppError> {
    let decoded_cursor = cursor.and_then(decode_cursor);

    let sql = if decoded_cursor.is_some() {
        adapt_sql(
            "SELECT s.did, s.authority_did, s.type_nsid, s.skey, sm.created_at FROM happyview_space_members sm JOIN happyview_spaces s ON s.id = sm.space_id WHERE sm.member_did = ? AND (sm.created_at > ? OR (sm.created_at = ? AND ('at://' || s.did || '/space/' || s.type_nsid || '/' || s.skey) > ?)) ORDER BY sm.created_at ASC, ('at://' || s.did || '/space/' || s.type_nsid || '/' || s.skey) ASC LIMIT ?",
            backend,
        )
    } else {
        adapt_sql(
            "SELECT s.did, s.authority_did, s.type_nsid, s.skey, sm.created_at FROM happyview_space_members sm JOIN happyview_spaces s ON s.id = sm.space_id WHERE sm.member_did = ? ORDER BY sm.created_at ASC, ('at://' || s.did || '/space/' || s.type_nsid || '/' || s.skey) ASC LIMIT ?",
            backend,
        )
    };

    let mut query = sqlx::query_as::<_, (String, String, String, String, String)>(&sql).bind(did);
    if let Some((ref ts, ref uri)) = decoded_cursor {
        query = query.bind(ts.as_str()).bind(ts.as_str()).bind(uri.as_str());
    }
    query = query.bind(limit);

    let rows = query
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list spaces for user: {e}")))?;

    let views: Vec<SpaceView> = rows
        .into_iter()
        .map(
            |(space_did, authority_did, type_nsid, skey, created_at)| SpaceView {
                uri: format!("at://{}/space/{}/{}", space_did, type_nsid, skey),
                is_owner: authority_did == did,
                created_at,
            },
        )
        .collect();

    let next_cursor = if views.len() as i64 == limit {
        views.last().map(|v| encode_cursor(&v.created_at, &v.uri))
    } else {
        None
    };

    Ok((views, next_cursor))
}

pub async fn update_space(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space: &Space,
) -> Result<bool, AppError> {
    let now = now_rfc3339();
    let config_json = serde_json::to_string(&space.config)
        .map_err(|e| AppError::Internal(format!("failed to serialize space config: {e}")))?;
    let app_access_json = serde_json::to_string(&space.app_access)
        .map_err(|e| AppError::Internal(format!("failed to serialize app_access: {e}")))?;

    let sql = adapt_sql(
        "UPDATE happyview_spaces SET display_name = ?, description = ?, mint_policy = ?, app_access = ?, managing_app_did = ?, config = ?, updated_at = ? WHERE id = ?",
        backend,
    );

    let result = sqlx::query(&sql)
        .bind(&space.display_name)
        .bind(&space.description)
        .bind(space.mint_policy.as_str())
        .bind(&app_access_json)
        .bind(&space.managing_app_did)
        .bind(&config_json)
        .bind(&now)
        .bind(&space.id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update space: {e}")))?;

    Ok(result.rows_affected() > 0)
}

pub async fn delete_space(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<bool, AppError> {
    let sql = adapt_sql("DELETE FROM happyview_spaces WHERE id = ?", backend);

    let result = sqlx::query(&sql)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete space: {e}")))?;

    Ok(result.rows_affected() > 0)
}

type SpaceRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
    String,
);

fn parse_space_row(r: SpaceRow) -> Result<Space, AppError> {
    let mint_policy = MintPolicy::parse(&r.8)
        .ok_or_else(|| AppError::Internal(format!("invalid mint_policy: {}", r.8)))?;
    let app_access: AppAccess = serde_json::from_str(&r.9)
        .map_err(|e| AppError::Internal(format!("invalid app_access: {e}")))?;
    let config: SpaceConfig = serde_json::from_str(&r.11)
        .map_err(|e| AppError::Internal(format!("invalid space config: {e}")))?;

    Ok(Space {
        id: r.0,
        did: r.1,
        authority_did: r.2,
        creator_did: r.3,
        type_nsid: r.4,
        skey: r.5,
        display_name: r.6,
        description: r.7,
        mint_policy,
        app_access,
        managing_app_did: r.10,
        config,
        revision: r.12,
        created_at: r.13,
        updated_at: r.14,
    })
}

// ---------------------------------------------------------------------------
// Space Members
// ---------------------------------------------------------------------------

pub async fn add_member(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    member: &SpaceMember,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_space_members (id, space_id, member_did, access, is_delegation, granted_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        backend,
    );

    sqlx::query(&sql)
        .bind(&member.id)
        .bind(&member.space_id)
        .bind(&member.did)
        .bind(member.access.as_str())
        .bind(member.is_delegation as i32)
        .bind(&member.granted_by)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to add member: {e}")))?;

    Ok(())
}

pub async fn remove_member(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    did: &str,
) -> Result<bool, AppError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_space_members WHERE space_id = ? AND member_did = ?",
        backend,
    );

    let result = sqlx::query(&sql)
        .bind(space_id)
        .bind(did)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to remove member: {e}")))?;

    Ok(result.rows_affected() > 0)
}

pub async fn get_member(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    did: &str,
) -> Result<Option<SpaceMember>, AppError> {
    let sql = adapt_sql(
        "SELECT id, space_id, member_did, access, is_delegation, granted_by, created_at FROM happyview_space_members WHERE space_id = ? AND member_did = ?",
        backend,
    );

    let row: Option<MemberRow> = sqlx::query_as(&sql)
        .bind(space_id)
        .bind(did)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get member: {e}")))?;

    row.map(parse_member_row).transpose()
}

pub async fn list_direct_members(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Vec<SpaceMember>, AppError> {
    let sql = adapt_sql(
        "SELECT id, space_id, member_did, access, is_delegation, granted_by, created_at FROM happyview_space_members WHERE space_id = ? ORDER BY created_at ASC",
        backend,
    );

    let rows: Vec<MemberRow> = sqlx::query_as(&sql)
        .bind(space_id)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list members: {e}")))?;

    rows.into_iter().map(parse_member_row).collect()
}

pub async fn list_spaces_for_member(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    did: &str,
) -> Result<Vec<SpaceMember>, AppError> {
    let sql = adapt_sql(
        "SELECT id, space_id, member_did, access, is_delegation, granted_by, created_at FROM happyview_space_members WHERE member_did = ? ORDER BY created_at ASC",
        backend,
    );

    let rows: Vec<MemberRow> = sqlx::query_as(&sql)
        .bind(did)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list spaces for member: {e}")))?;

    rows.into_iter().map(parse_member_row).collect()
}

type MemberRow = (String, String, String, String, i32, Option<String>, String);

fn parse_member_row(r: MemberRow) -> Result<SpaceMember, AppError> {
    let access = SpaceAccess::parse(&r.3)
        .ok_or_else(|| AppError::Internal(format!("invalid access: {}", r.3)))?;

    Ok(SpaceMember {
        id: r.0,
        space_id: r.1,
        did: r.2,
        access,
        is_delegation: r.4 != 0,
        granted_by: r.5,
        created_at: r.6,
    })
}

// ---------------------------------------------------------------------------
// Space Records
// ---------------------------------------------------------------------------

pub async fn upsert_space_record(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    record: &SpaceRecord,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let record_json = serde_json::to_string(&record.record)
        .map_err(|e| AppError::Internal(format!("failed to serialize record: {e}")))?;

    let sql = match backend {
        DatabaseBackend::Sqlite => {
            "INSERT OR REPLACE INTO happyview_space_records (uri, space_id, author_did, collection, rkey, record, cid, indexed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)".to_string()
        }
        DatabaseBackend::Postgres => adapt_sql(
            "INSERT INTO happyview_space_records (uri, space_id, author_did, collection, rkey, record, cid, indexed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (uri) DO UPDATE SET record = EXCLUDED.record, cid = EXCLUDED.cid, indexed_at = EXCLUDED.indexed_at",
            backend,
        ),
    };

    sqlx::query(&sql)
        .bind(&record.uri)
        .bind(&record.space_id)
        .bind(&record.author_did)
        .bind(&record.collection)
        .bind(&record.rkey)
        .bind(&record_json)
        .bind(&record.cid)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to upsert space record: {e}")))?;

    Ok(())
}

pub async fn get_space_record(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    uri: &str,
) -> Result<Option<SpaceRecord>, AppError> {
    let sql = adapt_sql(
        "SELECT uri, space_id, author_did, collection, rkey, record, cid, indexed_at FROM happyview_space_records WHERE uri = ?",
        backend,
    );

    let row: Option<RecordRow> = sqlx::query_as(&sql)
        .bind(uri)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get space record: {e}")))?;

    row.map(parse_record_row).transpose()
}

pub async fn get_space_record_by_parts(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    collection: &str,
    rkey: &str,
) -> Result<Option<SpaceRecord>, AppError> {
    let sql = adapt_sql(
        "SELECT uri, space_id, author_did, collection, rkey, record, cid, indexed_at FROM happyview_space_records WHERE space_id = ? AND collection = ? AND rkey = ? LIMIT 1",
        backend,
    );

    let row: Option<RecordRow> = sqlx::query_as(&sql)
        .bind(space_id)
        .bind(collection)
        .bind(rkey)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get space record: {e}")))?;

    row.map(parse_record_row).transpose()
}

#[allow(clippy::too_many_arguments)]
pub async fn list_space_records(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    repo: Option<&str>,
    collection: Option<&str>,
    limit: i64,
    cursor: Option<&str>,
    reverse: bool,
) -> Result<(Vec<SpaceRecord>, Option<String>), AppError> {
    let decoded_cursor = cursor.and_then(decode_cursor);

    let mut conditions = vec!["space_id = ?".to_string()];
    if repo.is_some() {
        conditions.push("author_did = ?".to_string());
    }
    if collection.is_some() {
        conditions.push("collection = ?".to_string());
    }
    let (cursor_cmp, order) = if reverse { ("<", "DESC") } else { (">", "ASC") };
    if decoded_cursor.is_some() {
        conditions.push(format!(
            "(indexed_at {cursor_cmp} ? OR (indexed_at = ? AND uri {cursor_cmp} ?))"
        ));
    }

    let where_clause = conditions.join(" AND ");
    let raw = format!(
        "SELECT uri, space_id, author_did, collection, rkey, record, cid, indexed_at FROM happyview_space_records WHERE {} ORDER BY indexed_at {}, uri {} LIMIT ?",
        where_clause, order, order
    );
    let sql = adapt_sql(&raw, backend);

    let mut query = sqlx::query_as::<_, RecordRow>(&sql).bind(space_id);
    if let Some(r) = repo {
        query = query.bind(r);
    }
    if let Some(c) = collection {
        query = query.bind(c);
    }
    if let Some((ref ts, ref uri)) = decoded_cursor {
        query = query.bind(ts.as_str()).bind(ts.as_str()).bind(uri.as_str());
    }
    query = query.bind(limit);

    let rows = query
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list space records: {e}")))?;

    let records: Vec<SpaceRecord> = rows
        .into_iter()
        .map(parse_record_row)
        .collect::<Result<_, _>>()?;

    let next_cursor = if records.len() as i64 == limit {
        records.last().map(|r| encode_cursor(&r.indexed_at, &r.uri))
    } else {
        None
    };

    Ok((records, next_cursor))
}

pub async fn list_all_space_records(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    author_did: &str,
) -> Result<Vec<SpaceRecord>, AppError> {
    let sql = adapt_sql(
        "SELECT uri, space_id, author_did, collection, rkey, record, cid, indexed_at FROM happyview_space_records WHERE space_id = ? AND author_did = ? ORDER BY collection, rkey",
        backend,
    );

    let rows: Vec<RecordRow> = sqlx::query_as(&sql)
        .bind(space_id)
        .bind(author_did)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list all space records: {e}")))?;

    rows.into_iter().map(parse_record_row).collect()
}

pub async fn insert_space_record(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    record: &SpaceRecord,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let record_json = serde_json::to_string(&record.record)
        .map_err(|e| AppError::Internal(format!("failed to serialize record: {e}")))?;

    let sql = adapt_sql(
        "INSERT INTO happyview_space_records (uri, space_id, author_did, collection, rkey, record, cid, indexed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        backend,
    );

    sqlx::query(&sql)
        .bind(&record.uri)
        .bind(&record.space_id)
        .bind(&record.author_did)
        .bind(&record.collection)
        .bind(&record.rkey)
        .bind(&record_json)
        .bind(&record.cid)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("UNIQUE") || msg.contains("duplicate") || msg.contains("unique") {
                AppError::Conflict("Record already exists".into())
            } else {
                AppError::Internal(format!("failed to create space record: {e}"))
            }
        })?;

    Ok(())
}

pub async fn upsert_space_record_with_swap(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    record: &SpaceRecord,
    swap_cid: &str,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let record_json = serde_json::to_string(&record.record)
        .map_err(|e| AppError::Internal(format!("failed to serialize record: {e}")))?;

    let sql = adapt_sql(
        "UPDATE happyview_space_records SET record = ?, cid = ?, indexed_at = ? WHERE uri = ? AND cid = ?",
        backend,
    );

    let result = sqlx::query(&sql)
        .bind(&record_json)
        .bind(&record.cid)
        .bind(&now)
        .bind(&record.uri)
        .bind(swap_cid)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update space record: {e}")))?;

    if result.rows_affected() == 0 {
        let existing = get_space_record(pool, backend, &record.uri).await?;
        if existing.is_some() {
            return Err(AppError::Conflict("Record CID mismatch".into()));
        }
        return Err(AppError::NotFound("Record not found".into()));
    }

    Ok(())
}

pub async fn delete_space_record(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    uri: &str,
) -> Result<bool, AppError> {
    let sql = adapt_sql("DELETE FROM happyview_space_records WHERE uri = ?", backend);

    let result = sqlx::query(&sql)
        .bind(uri)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete space record: {e}")))?;

    Ok(result.rows_affected() > 0)
}

pub async fn delete_space_record_with_swap(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    uri: &str,
    swap_cid: &str,
) -> Result<bool, AppError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_space_records WHERE uri = ? AND cid = ?",
        backend,
    );

    let result = sqlx::query(&sql)
        .bind(uri)
        .bind(swap_cid)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete space record: {e}")))?;

    if result.rows_affected() == 0 {
        let existing = get_space_record(pool, backend, uri).await?;
        if existing.is_some() {
            return Err(AppError::Conflict("Record CID mismatch".into()));
        }
        return Err(AppError::NotFound("Record not found".into()));
    }

    Ok(true)
}

pub async fn update_space_revision(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    revision: &str,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_spaces SET revision = ?, updated_at = ? WHERE id = ?",
        backend,
    );

    sqlx::query(&sql)
        .bind(revision)
        .bind(&now)
        .bind(space_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update space revision: {e}")))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Repo State
// ---------------------------------------------------------------------------

pub async fn get_or_create_repo_state(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    author_did: &str,
) -> Result<RepoState, AppError> {
    let sql = adapt_sql(
        "SELECT id, space_id, author_did, lthash_state, rev, hash, ikm, sig, mac, updated_at FROM happyview_space_repo_state WHERE space_id = ? AND author_did = ?",
        backend,
    );

    let row: Option<RepoStateRow> = sqlx::query_as(&sql)
        .bind(space_id)
        .bind(author_did)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get repo state: {e}")))?;

    if let Some(r) = row {
        return parse_repo_state_row(r);
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let default_lthash = vec![0u8; 2048];
    let insert_sql = adapt_sql(
        "INSERT INTO happyview_space_repo_state (id, space_id, author_did, lthash_state, rev, hash, ikm, sig, mac, updated_at) VALUES (?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, ?)",
        backend,
    );
    sqlx::query(&insert_sql)
        .bind(&id)
        .bind(space_id)
        .bind(author_did)
        .bind(&default_lthash)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create repo state: {e}")))?;

    Ok(RepoState {
        id,
        space_id: space_id.to_string(),
        author_did: author_did.to_string(),
        lthash_state: default_lthash,
        rev: None,
        hash: None,
        ikm: None,
        sig: None,
        mac: None,
        updated_at: now,
    })
}

pub async fn update_repo_state(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    state: &RepoState,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_space_repo_state SET lthash_state = ?, rev = ?, hash = ?, ikm = ?, sig = ?, mac = ?, updated_at = ? WHERE id = ?",
        backend,
    );

    sqlx::query(&sql)
        .bind(&state.lthash_state)
        .bind(&state.rev)
        .bind(&state.hash)
        .bind(&state.ikm)
        .bind(&state.sig)
        .bind(&state.mac)
        .bind(&now)
        .bind(&state.id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update repo state: {e}")))?;

    Ok(())
}

type RepoStateRow = (
    String,
    String,
    String,
    Vec<u8>,
    Option<String>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    String,
);

fn parse_repo_state_row(r: RepoStateRow) -> Result<RepoState, AppError> {
    Ok(RepoState {
        id: r.0,
        space_id: r.1,
        author_did: r.2,
        lthash_state: r.3,
        rev: r.4,
        hash: r.5,
        ikm: r.6,
        sig: r.7,
        mac: r.8,
        updated_at: r.9,
    })
}

// ---------------------------------------------------------------------------
// Notification Registrations
// ---------------------------------------------------------------------------

pub async fn register_notify(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    reg: &NotifyRegistration,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_space_notify_registrations (id, space_id, author_did, endpoint, registered_by, expires_at, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        backend,
    );

    sqlx::query(&sql)
        .bind(&reg.id)
        .bind(&reg.space_id)
        .bind(&reg.author_did)
        .bind(&reg.endpoint)
        .bind(&reg.registered_by)
        .bind(&reg.expires_at)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to register notify: {e}")))?;

    Ok(())
}

pub async fn list_notify_registrations(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    author_did: Option<&str>,
) -> Result<Vec<NotifyRegistration>, AppError> {
    let sql = if author_did.is_some() {
        adapt_sql(
            "SELECT id, space_id, author_did, endpoint, registered_by, expires_at, created_at FROM happyview_space_notify_registrations WHERE space_id = ? AND author_did = ? ORDER BY created_at ASC",
            backend,
        )
    } else {
        adapt_sql(
            "SELECT id, space_id, author_did, endpoint, registered_by, expires_at, created_at FROM happyview_space_notify_registrations WHERE space_id = ? ORDER BY created_at ASC",
            backend,
        )
    };

    let mut query = sqlx::query_as::<_, NotifyRow>(&sql).bind(space_id);
    if let Some(did) = author_did {
        query = query.bind(did);
    }

    let rows = query
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list notify registrations: {e}")))?;

    Ok(rows.into_iter().map(parse_notify_row).collect())
}

pub async fn delete_notify_registration(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<bool, AppError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_space_notify_registrations WHERE id = ?",
        backend,
    );

    let result = sqlx::query(&sql)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete notify registration: {e}")))?;

    Ok(result.rows_affected() > 0)
}

type NotifyRow = (
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
);

fn parse_notify_row(r: NotifyRow) -> NotifyRegistration {
    NotifyRegistration {
        id: r.0,
        space_id: r.1,
        author_did: r.2,
        endpoint: r.3,
        registered_by: r.4,
        expires_at: r.5,
        created_at: r.6,
    }
}

type RecordRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

fn parse_record_row(r: RecordRow) -> Result<SpaceRecord, AppError> {
    let record: serde_json::Value = serde_json::from_str(&r.5)
        .map_err(|e| AppError::Internal(format!("invalid record JSON: {e}")))?;

    Ok(SpaceRecord {
        uri: r.0,
        space_id: r.1,
        author_did: r.2,
        collection: r.3,
        rkey: r.4,
        record,
        cid: r.6,
        indexed_at: r.7,
    })
}

/// Find the author DID of any record in the space that contains a blob ref
/// with the given CID. The CID appears in serialised record JSON as the
/// `$link` value inside an ATProto blob ref object.
pub async fn find_blob_author_did(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    blob_cid: &str,
) -> Result<Option<String>, AppError> {
    let pattern = format!("%\"$link\":\"{blob_cid}\"%");
    let sql = adapt_sql(
        "SELECT author_did FROM happyview_space_records WHERE space_id = ? AND record LIKE ? LIMIT 1",
        backend,
    );
    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind(space_id)
        .bind(&pattern)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to find blob author: {e}")))?;
    Ok(row.map(|(did,)| did))
}

// ---------------------------------------------------------------------------
// Space Repos
// ---------------------------------------------------------------------------

pub async fn list_space_repos(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let sql = adapt_sql(
        "SELECT DISTINCT r.author_did, s.rev FROM happyview_space_records r LEFT JOIN happyview_space_repo_state s ON s.space_id = r.space_id AND s.author_did = r.author_did WHERE r.space_id = ? ORDER BY r.author_did ASC",
        backend,
    );

    let rows: Vec<(String, Option<String>)> = sqlx::query_as(&sql)
        .bind(space_id)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list space repos: {e}")))?;

    Ok(rows
        .into_iter()
        .map(|(did, rev)| serde_json::json!({ "did": did, "rev": rev }))
        .collect())
}

// ---------------------------------------------------------------------------
// Space Invites
// ---------------------------------------------------------------------------

pub async fn create_invite(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    invite: &SpaceInvite,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_space_invites (id, space_id, token_hash, created_by, access, max_uses, uses, expires_at, revoked, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        backend,
    );

    sqlx::query(&sql)
        .bind(&invite.id)
        .bind(&invite.space_id)
        .bind(&invite.token_hash)
        .bind(&invite.created_by)
        .bind(invite.access.as_str())
        .bind(invite.max_uses)
        .bind(invite.uses)
        .bind(&invite.expires_at)
        .bind(invite.revoked as i32)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create invite: {e}")))?;

    Ok(())
}

pub async fn get_invite_by_token_hash(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    token_hash: &str,
) -> Result<Option<SpaceInvite>, AppError> {
    let sql = adapt_sql(
        "SELECT id, space_id, token_hash, created_by, access, max_uses, uses, expires_at, revoked, created_at FROM happyview_space_invites WHERE token_hash = ?",
        backend,
    );

    let row: Option<InviteRow> = sqlx::query_as(&sql)
        .bind(token_hash)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get invite: {e}")))?;

    row.map(parse_invite_row).transpose()
}

pub async fn increment_invite_uses(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    invite_id: &str,
) -> Result<(), AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_space_invites SET uses = uses + 1 WHERE id = ?",
        backend,
    );

    sqlx::query(&sql)
        .bind(invite_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to increment invite uses: {e}")))?;

    Ok(())
}

pub async fn revoke_invite(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    invite_id: &str,
) -> Result<bool, AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_space_invites SET revoked = 1 WHERE id = ?",
        backend,
    );

    let result = sqlx::query(&sql)
        .bind(invite_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke invite: {e}")))?;

    Ok(result.rows_affected() > 0)
}

pub async fn list_invites(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Vec<SpaceInvite>, AppError> {
    let sql = adapt_sql(
        "SELECT id, space_id, token_hash, created_by, access, max_uses, uses, expires_at, revoked, created_at FROM happyview_space_invites WHERE space_id = ? ORDER BY created_at DESC",
        backend,
    );

    let rows: Vec<InviteRow> = sqlx::query_as(&sql)
        .bind(space_id)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list invites: {e}")))?;

    rows.into_iter().map(parse_invite_row).collect()
}

type InviteRow = (
    String,
    String,
    String,
    String,
    String,
    Option<i64>,
    i64,
    Option<String>,
    i32,
    String,
);

fn parse_invite_row(r: InviteRow) -> Result<SpaceInvite, AppError> {
    let access = SpaceAccess::parse(&r.4)
        .ok_or_else(|| AppError::Internal(format!("invalid invite access: {}", r.4)))?;

    Ok(SpaceInvite {
        id: r.0,
        space_id: r.1,
        token_hash: r.2,
        created_by: r.3,
        access,
        max_uses: r.5,
        uses: r.6,
        expires_at: r.7,
        revoked: r.8 != 0,
        created_at: r.9,
    })
}
