//! Bytes addressed by the CID of their content.
//!
//! The CID is the row's identity, so the checksum a caller verifies and the
//! key the row is filed under are one fact rather than two that can disagree.
//! Storing the same content twice is a no-op for the same reason.

use serde::Serialize;

use crate::cid_verify;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;

/// A stored blob, with the two things a response needs beside its bytes.
#[derive(Debug, Clone)]
pub struct Blob {
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

/// What a blob looks like in a record or a script's return value: atproto's
/// own blob ref, which is what `record_refs::extract_blob_cids` already
/// matches and what a lexicon's `blob` type describes.
#[derive(Debug, Clone, Serialize)]
pub struct BlobRef {
    #[serde(rename = "$type")]
    pub kind: &'static str,
    #[serde(rename = "ref")]
    pub link: BlobLink,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlobLink {
    #[serde(rename = "$link")]
    pub link: String,
}

impl BlobRef {
    pub fn new(cid: String, mime_type: String, size: i64) -> Self {
        Self {
            kind: "blob",
            link: BlobLink { link: cid },
            mime_type,
            size,
        }
    }
}

/// Read a blob's bytes and media type.
pub async fn get(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    cid: &str,
) -> Result<Option<Blob>, AppError> {
    let sql = adapt_sql(
        "SELECT bytes, mime_type FROM happyview_blobs WHERE cid = ?",
        backend,
    );
    let row: Option<(Vec<u8>, String)> = crate::db::query_as(&sql)
        .bind(cid)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read blob: {e}")))?;

    Ok(row.map(|(bytes, mime_type)| Blob { bytes, mime_type }))
}

/// Store bytes and answer the CID they are filed under.
///
/// The caller does not choose the key: it is computed from the content, so
/// bytes cannot be filed under a CID they do not hash to. A second write of
/// the same content keeps the row already there, which also means the stored
/// `mime_type` is the first writer's.
pub async fn put(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    bytes: &[u8],
    mime_type: &str,
) -> Result<String, AppError> {
    let cid = cid_verify::raw_cid(bytes)
        .ok_or_else(|| AppError::Internal("failed to compute a blob CID".into()))?
        .to_string();

    let sql = adapt_sql(
        "INSERT INTO happyview_blobs (cid, bytes, mime_type, size, created_at) \
         VALUES (?, ?, ?, ?, ?) ON CONFLICT (cid) DO NOTHING",
        backend,
    );
    crate::db::query(&sql)
        .bind(&cid)
        .bind(bytes)
        .bind(mime_type)
        .bind(bytes.len() as i64)
        .bind(now_rfc3339())
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to store blob: {e}")))?;

    Ok(cid)
}
