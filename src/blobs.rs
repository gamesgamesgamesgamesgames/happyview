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

/// What a blob is, without its bytes. `None` for an absent one is also how a
/// caller asks whether it is held at all, so there is no separate existence
/// query to drift from this one.
#[derive(Debug, Clone)]
pub struct BlobStat {
    pub mime_type: String,
    pub size: i64,
}

/// A blob's media type and size without transferring it.
pub async fn stat(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    cid: &str,
) -> Result<Option<BlobStat>, AppError> {
    let sql = adapt_sql(
        "SELECT mime_type, size FROM happyview_blobs WHERE cid = ?",
        backend,
    );
    let row: Option<(String, i64)> = crate::db::query_as(&sql)
        .bind(cid)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to stat blob: {e}")))?;

    Ok(row.map(|(mime_type, size)| BlobStat { mime_type, size }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::migrated_memory_pool;

    const BACKEND: DatabaseBackend = DatabaseBackend::Sqlite;

    #[tokio::test]
    async fn bytes_round_trip_under_the_cid_the_network_would_mint() {
        let pool = migrated_memory_pool().await;
        // Not valid UTF-8, so nothing can be quietly treating this as text.
        let bytes: &[u8] = &[0x00, 0x61, 0xff, 0x73, 0x6d];

        let cid = put(&pool, BACKEND, bytes, "application/wasm")
            .await
            .expect("store");
        assert_eq!(
            cid,
            cid_verify::raw_cid(bytes).expect("cid").to_string(),
            "the key has to be the CID of the content"
        );

        let blob = get(&pool, BACKEND, &cid)
            .await
            .expect("read")
            .expect("stored");
        assert_eq!(blob.bytes, bytes);
        assert_eq!(blob.mime_type, "application/wasm");

        let stat = stat(&pool, BACKEND, &cid)
            .await
            .expect("stat")
            .expect("stored");
        assert_eq!(stat.size, bytes.len() as i64);
        assert_eq!(stat.mime_type, "application/wasm");
    }

    /// Content addressing is what makes a second write of the same bytes a
    /// no-op rather than a conflict or a duplicate row.
    #[tokio::test]
    async fn storing_the_same_content_twice_keeps_one_row() {
        let pool = migrated_memory_pool().await;

        let first = put(&pool, BACKEND, b"same", "application/wasm")
            .await
            .expect("first");
        let second = put(&pool, BACKEND, b"same", "text/plain")
            .await
            .expect("second");
        assert_eq!(first, second);

        let rows: i64 = crate::db::query_as::<(i64,)>(&adapt_sql(
            "SELECT COUNT(*) FROM happyview_blobs",
            BACKEND,
        ))
        .fetch_one(&pool)
        .await
        .expect("count")
        .0;
        assert_eq!(rows, 1);

        // The first writer's media type stands, as the column's comment says.
        let blob = get(&pool, BACKEND, &first)
            .await
            .expect("read")
            .expect("stored");
        assert_eq!(blob.mime_type, "application/wasm");
    }

    /// Absent is `None` from both reads, which is how a caller asks whether a
    /// blob is held at all.
    #[tokio::test]
    async fn an_unstored_cid_is_absent_rather_than_an_error() {
        let pool = migrated_memory_pool().await;
        let cid = cid_verify::raw_cid(b"never stored")
            .expect("cid")
            .to_string();

        assert!(get(&pool, BACKEND, &cid).await.expect("read").is_none());
        assert!(stat(&pool, BACKEND, &cid).await.expect("stat").is_none());
    }
}
