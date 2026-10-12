//! Record bodies on SQLite: zstd level 3 with a per-collection dictionary.
//!
//! A collection starts raw. Once it has a dictionary (trained by the
//! maintenance task, see `train`), new writes compress with the newest one,
//! and the background recompression compresses rows stored raw. A body is
//! always the JSON text exactly as received; compression never changes a
//! byte of what `hv_record` hands back. Postgres stores `record` as TEXT and
//! never comes here.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures_util::TryStreamExt;
use sqlx::AnyPool;

use crate::db::DatabaseBackend;
use crate::record_store::RecordStore;

pub const ZSTD_LEVEL: i32 = 3;

/// Bodies shorter than this are stored raw: zstd's frame header alone eats
/// most of what a dictionary would save.
pub const MIN_COMPRESS_BYTES: usize = 64;

/// `body_dict` for a body stored raw after compression was considered.
pub const RAW_CONSIDERED: i64 = 0;

/// The largest body `decompress` will produce; a repo record is at most
/// 1 MiB, so this only stops a corrupt or hostile frame.
pub const MAX_RECORD_BYTES: usize = 16 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    Zstd,
    None,
}

/// `RECORD_COMPRESSION`: `zstd` (the default) or `none`. Anything else falls
/// back to `zstd` with a warning rather than failing boot over a tuning knob.
pub fn parse_compression(raw: Option<&str>) -> Compression {
    match raw.map(str::trim) {
        None | Some("") => Compression::Zstd,
        Some(v) if v.eq_ignore_ascii_case("zstd") => Compression::Zstd,
        Some(v) if v.eq_ignore_ascii_case("none") => Compression::None,
        Some(v) => {
            tracing::warn!(
                value = v,
                "RECORD_COMPRESSION must be zstd or none; using zstd"
            );
            Compression::Zstd
        }
    }
}

pub fn compression_from_env() -> Compression {
    parse_compression(std::env::var("RECORD_COMPRESSION").ok().as_deref())
}

struct CurrentDict {
    id: i64,
    encoder: zstd::dict::EncoderDictionary<'static>,
}

/// Encodes bodies for writing: the compression mode and each collection's
/// newest dictionary.
pub struct BodyCodec {
    mode: Compression,
    current: RwLock<HashMap<i64, Arc<CurrentDict>>>,
}

impl BodyCodec {
    /// Writes every body raw (`body_dict` NULL) for the background task to
    /// compress: what a store without a loaded codec uses.
    pub fn disabled() -> Self {
        Self::new(Compression::None)
    }

    pub fn new(mode: Compression) -> Self {
        Self {
            mode,
            current: RwLock::new(HashMap::new()),
        }
    }

    /// A codec holding each collection's newest stored dictionary (none when
    /// `mode` is `None`).
    pub async fn load(pool: &AnyPool, mode: Compression) -> Result<Self, sqlx::Error> {
        let codec = Self::new(mode);
        if mode == Compression::Zstd {
            let rows: Vec<(i64, i64, Vec<u8>)> = crate::db::query_as(
                "SELECT z.id, z.collection_id, z.dict FROM happyview_zstd_dicts z \
                 WHERE z.id = (SELECT MAX(id) FROM happyview_zstd_dicts WHERE collection_id = z.collection_id)",
            )
            .fetch_all(pool)
            .await?;
            for (id, collection_id, dict) in rows {
                codec.install(collection_id, id, &dict);
            }
        }
        Ok(codec)
    }

    pub fn mode(&self) -> Compression {
        self.mode
    }

    /// Make `dict` (stored as `dict_id`) the one new writes to
    /// `collection_id` use.
    pub fn install(&self, collection_id: i64, dict_id: i64, dict: &[u8]) {
        let current = Arc::new(CurrentDict {
            id: dict_id,
            encoder: zstd::dict::EncoderDictionary::copy(dict, ZSTD_LEVEL),
        });
        self.current
            .write()
            .expect("dictionary cache poisoned")
            .insert(collection_id, current);
    }

    pub fn current_dict(&self, collection_id: i64) -> Option<i64> {
        if self.mode == Compression::None {
            return None;
        }
        self.current
            .read()
            .expect("dictionary cache poisoned")
            .get(&collection_id)
            .map(|d| d.id)
    }

    /// `(body, body_dict)` for `json`: compressed with the collection's
    /// newest dictionary when that helps and the body is one `decompress`
    /// will read back, raw otherwise.
    pub fn encode(&self, collection_id: i64, json: &str) -> (Vec<u8>, Option<i64>) {
        let raw = json.as_bytes().to_vec();
        if self.mode == Compression::None {
            return (raw, None);
        }
        let Some(dict) = self
            .current
            .read()
            .expect("dictionary cache poisoned")
            .get(&collection_id)
            .cloned()
        else {
            return (raw, None);
        };
        // Past MAX_RECORD_BYTES `decompress` would refuse the frame, and
        // every read of the row would fail.
        if raw.len() < MIN_COMPRESS_BYTES || raw.len() > MAX_RECORD_BYTES {
            return (raw, Some(RAW_CONSIDERED));
        }
        match zstd::bulk::Compressor::with_prepared_dictionary(&dict.encoder)
            .and_then(|mut compressor| compressor.compress(&raw))
        {
            Ok(compressed) if compressed.len() < raw.len() => (compressed, Some(dict.id)),
            _ => (raw, Some(RAW_CONSIDERED)),
        }
    }
}

/// Decode one stored body with its dictionary.
pub fn decompress(body: &[u8], dict: &zstd::dict::DecoderDictionary<'_>) -> Result<String, String> {
    let decoder = zstd::stream::read::Decoder::with_prepared_dictionary(body, dict)
        .map_err(|e| e.to_string())?
        .single_frame();
    let mut out = Vec::new();
    decoder
        .take(MAX_RECORD_BYTES as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() > MAX_RECORD_BYTES {
        return Err(format!("body decodes past {MAX_RECORD_BYTES} bytes"));
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}

/// Records a collection needs before it gets a dictionary.
pub const DEFAULT_TRAIN_AT: i64 = 5_000;
/// Newest bodies a dictionary is trained from.
pub const TRAIN_SAMPLES: i64 = 20_000;
/// Dictionary size.
pub const DICT_BYTES: usize = 64 * 1024;
/// Largest single body a dictionary is trained from; past this a record
/// is an outlier that would only crowd out the typical ones.
pub const TRAIN_SAMPLE_MAX_BYTES: i64 = 1 << 20;
/// Total sample bytes held for one training. zstd gains nothing from much
/// more than ~100× the dictionary size, and this bounds the memory a
/// collection of large records costs (zstd copies the samples once more).
pub const TRAIN_BUDGET_BYTES: usize = 32 << 20;
/// Rows per recompression transaction, so ingest interleaves.
pub const RECOMPRESS_BATCH: i64 = 1_000;
/// Body bytes per recompression transaction, so a collection of large
/// records holds neither the write lock nor memory for long.
pub const RECOMPRESS_BUDGET_BYTES: usize = 32 << 20;
/// Seconds between maintenance passes.
pub const DEFAULT_INTERVAL_SECS: u64 = 600;

/// `ZSTD_DICT_TRAIN_AT`; a missing, unparseable or non-positive value is the
/// default.
pub fn parse_train_at(raw: Option<&str>) -> i64 {
    raw.and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_TRAIN_AT)
}

pub fn train_at_from_env() -> i64 {
    parse_train_at(std::env::var("ZSTD_DICT_TRAIN_AT").ok().as_deref())
}

/// `RECORD_COMPRESSION_INTERVAL_SECS`, the seconds between passes.
pub fn parse_interval_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_INTERVAL_SECS)
}

pub fn interval_from_env() -> Duration {
    Duration::from_secs(parse_interval_secs(
        std::env::var("RECORD_COMPRESSION_INTERVAL_SECS")
            .ok()
            .as_deref(),
    ))
}

/// The magic number that opens a zstd dictionary, little-endian.
pub const ZSTD_DICT_MAGIC: [u8; 4] = [0x37, 0xA4, 0x30, 0xEC];

/// Whether `dict` is a zstd-format dictionary rather than raw content.
pub fn is_zstd_dictionary(dict: &[u8]) -> bool {
    dict.starts_with(&ZSTD_DICT_MAGIC)
}

#[derive(Debug, thiserror::Error)]
pub enum TrainError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error("the collection has no records to train on")]
    NoSamples,
    #[error("zstd could not train a dictionary: {0}")]
    Zstd(String),
}

/// Collections that hold at least `train_at` records and have no
/// dictionary. Each count stops at `train_at`, so a large collection costs
/// no more than a small one.
pub async fn due_for_training(pool: &AnyPool, train_at: i64) -> Result<Vec<i64>, sqlx::Error> {
    let candidates: Vec<(i64,)> = crate::db::query_as(
        "SELECT c.id FROM happyview_collections c \
         WHERE NOT EXISTS (SELECT 1 FROM happyview_zstd_dicts z WHERE z.collection_id = c.id) ORDER BY c.id",
    )
    .fetch_all(pool)
    .await?;
    let mut due = Vec::new();
    for (collection_id,) in candidates {
        let (held,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM (SELECT 1 FROM happyview_records_v2 WHERE collection_id = ? LIMIT ?) AS sample",
        )
        .bind(collection_id)
        .bind(train_at)
        .fetch_one(pool)
        .await?;
        if held >= train_at {
            due.push(collection_id);
        }
    }
    Ok(due)
}

/// Up to `TRAIN_SAMPLES` of `collection_id`'s newest bodies, newest first,
/// skipping any over `max_sample_bytes` and stopping before the total
/// passes `budget_bytes`. Rows stream in, so memory stays near the budget.
pub(crate) async fn training_samples(
    pool: &AnyPool,
    collection_id: i64,
    max_sample_bytes: i64,
    budget_bytes: usize,
) -> Result<Vec<String>, sqlx::Error> {
    // A stored body is never longer than its JSON, so `length(body)` rules
    // out the large ones without decoding them.
    let mut rows = crate::db::query_as::<(String,)>(
        "SELECT record FROM (\
           SELECT id, hv_record(body, body_dict) AS record FROM happyview_records_v2 \
           WHERE collection_id = ? AND length(body) <= ?\
         ) WHERE length(CAST(record AS BLOB)) <= ? ORDER BY id DESC LIMIT ?",
    )
    .bind(collection_id)
    .bind(max_sample_bytes)
    .bind(max_sample_bytes)
    .bind(TRAIN_SAMPLES)
    .fetch(pool);
    let (mut samples, mut total) = (Vec::new(), 0usize);
    while let Some((body,)) = rows.try_next().await? {
        if total + body.len() > budget_bytes {
            break;
        }
        total += body.len();
        samples.push(body);
    }
    Ok(samples)
}

/// Train a dictionary for `collection_id` from its newest bodies (see
/// `training_samples`), store it, and make new writes through `codec` use
/// it. The dictionary is committed on its own before anything references
/// it, so its id never names different bytes.
pub async fn train(
    pool: &AnyPool,
    codec: &BodyCodec,
    collection_id: i64,
    dict_bytes: usize,
) -> Result<i64, TrainError> {
    let samples = training_samples(
        pool,
        collection_id,
        TRAIN_SAMPLE_MAX_BYTES,
        TRAIN_BUDGET_BYTES,
    )
    .await?;
    if samples.is_empty() {
        return Err(TrainError::NoSamples);
    }
    let dict = tokio::task::spawn_blocking(move || zstd::dict::from_samples(&samples, dict_bytes))
        .await
        .map_err(|e| TrainError::Zstd(e.to_string()))?
        .map_err(|e| TrainError::Zstd(e.to_string()))?;
    // A raw-content dictionary (no magic) carries no id of its own, so a
    // frame read with the wrong one would decode to wrong text, silently.
    if !is_zstd_dictionary(&dict) {
        return Err(TrainError::Zstd(
            "training produced a raw-content dictionary, not a zstd one".into(),
        ));
    }
    let (dict_id,): (i64,) = crate::db::retry_on_busy(|| {
        crate::db::query_as(
            "INSERT INTO happyview_zstd_dicts (collection_id, created_at, dict) VALUES (?, ?, ?) RETURNING id",
        )
        .bind(collection_id)
        .bind(crate::record_store::codec::now_micros())
        .bind(dict.clone())
        .fetch_one(pool)
    })
    .await?;
    codec.install(collection_id, dict_id, &dict);
    Ok(dict_id)
}

/// What one recompression batch did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recompressed {
    /// Rows compressed or marked `RAW_CONSIDERED`.
    pub considered: u64,
    /// The batch stopped at its row or byte limit, so rows may be left.
    pub more: bool,
}

/// Compress up to `limit` of `collection_id`'s never-considered rows with
/// its current dictionary, in one write transaction, stopping once their
/// bodies total `budget_bytes` (always taking at least one row, so a batch
/// makes progress).
pub async fn recompress_batch(
    pool: &AnyPool,
    codec: &BodyCodec,
    collection_id: i64,
    limit: i64,
    budget_bytes: usize,
) -> Result<Recompressed, sqlx::Error> {
    if codec.current_dict(collection_id).is_none() {
        return Ok(Recompressed {
            considered: 0,
            more: false,
        });
    }
    crate::db::retry_on_busy(|| async {
        let mut tx = crate::db::begin_write(pool, DatabaseBackend::Sqlite).await?;
        let (mut rows, mut total, mut capped) = (Vec::<(i64, Vec<u8>)>::new(), 0usize, false);
        {
            let mut stream = crate::db::query_as::<(i64, Vec<u8>)>(
                "SELECT id, body FROM happyview_records_v2 \
                 WHERE collection_id = ? AND body_dict IS NULL ORDER BY id LIMIT ?",
            )
            .bind(collection_id)
            .bind(limit)
            .fetch(&mut *tx);
            while let Some(row) = stream.try_next().await? {
                total += row.1.len();
                rows.push(row);
                if total >= budget_bytes {
                    capped = true;
                    break;
                }
            }
        }
        for (id, body) in &rows {
            let (stored, dict) = match std::str::from_utf8(body) {
                Ok(json) => codec.encode(collection_id, json),
                Err(_) => (body.clone(), Some(RAW_CONSIDERED)),
            };
            crate::db::query(
                "UPDATE happyview_records_v2 SET body = ?, body_dict = ? WHERE id = ?",
            )
            .bind(stored)
            .bind(dict.unwrap_or(RAW_CONSIDERED))
            .bind(*id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(Recompressed {
            considered: rows.len() as u64,
            more: capped || rows.len() as i64 == limit,
        })
    })
    .await
}

/// One maintenance pass: train the collections that are due, recompress
/// every raw row of a collection with a dictionary, and hand freed pages
/// back to the filesystem. Returns the rows considered.
pub async fn pass(store: &RecordStore, train_at: i64) -> Result<u64, TrainError> {
    let (pool, codec) = (store.db(), store.bodies());
    for collection_id in due_for_training(pool, train_at).await? {
        match train(pool, codec, collection_id, DICT_BYTES).await {
            Ok(dict_id) => tracing::info!(collection_id, dict_id, "trained a zstd dictionary"),
            Err(e) => {
                tracing::warn!(collection_id, error = %e, "could not train a zstd dictionary")
            }
        }
    }
    let collections: Vec<(i64,)> = crate::db::query_as(
        "SELECT DISTINCT collection_id FROM happyview_zstd_dicts ORDER BY collection_id",
    )
    .fetch_all(pool)
    .await?;
    let mut considered = 0;
    for (collection_id,) in collections {
        loop {
            // One collection's bad row must not starve the ones after it.
            let batch = match recompress_batch(
                pool,
                codec,
                collection_id,
                RECOMPRESS_BATCH,
                RECOMPRESS_BUDGET_BYTES,
            )
            .await
            {
                Ok(batch) => batch,
                Err(e) => {
                    tracing::warn!(collection_id, error = %e, "could not recompress record bodies");
                    break;
                }
            };
            considered += batch.considered;
            if !batch.more {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    if considered > 0 {
        tracing::info!(considered, "recompressed record bodies");
        if let Err(e) = crate::db::query("PRAGMA incremental_vacuum")
            .execute(pool)
            .await
        {
            tracing::warn!(error = %e, "incremental vacuum after recompression failed");
        }
    }
    Ok(considered)
}

/// The maintenance loop: a pass shortly after boot, then every `every`.
/// Returns at once on Postgres or with `RECORD_COMPRESSION=none`.
pub async fn run(store: RecordStore, train_at: i64, every: Duration) {
    if store.backend() != DatabaseBackend::Sqlite || store.bodies().mode() != Compression::Zstd {
        return;
    }
    let first = tokio::time::Instant::now() + every.min(Duration::from_secs(30));
    let mut tick = tokio::time::interval_at(first, every);
    loop {
        tick.tick().await;
        if let Err(e) = pass(&store, train_at).await {
            tracing::warn!(error = %e, "record compression pass failed; retrying next interval");
        }
    }
}

#[cfg(test)]
pub(crate) fn sample_posts(n: usize) -> Vec<String> {
    let topics = [
        "sqlite",
        "postgres",
        "atproto",
        "lexicons",
        "jetstream",
        "backfill",
    ];
    (0..n)
        .map(|i| {
            format!(
                r#"{{"$type":"app.bsky.feed.post","text":"post {i} about {} — ünïcødé \"quoted\" \\ slash","createdAt":"2026-10-11T12:{:02}:{:02}.000Z","langs":["en"],"via":"witchsky"}}"#,
                topics[i % topics.len()],
                i % 60,
                (i * 7) % 60
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;

    fn train_dict(samples: &[String], size: usize) -> Vec<u8> {
        zstd::dict::from_samples(samples, size).expect("train a test dictionary")
    }

    /// Seed one collection, one DID and one dictionary, returning
    /// `(collection_id, did_id, dict_id)`.
    async fn seed(pool: &sqlx::AnyPool, dict: &[u8]) -> (i64, i64, i64) {
        let nsid = format!("app.test.zstd{}", uuid::Uuid::new_v4().simple());
        let (collection_id,): (i64,) =
            crate::db::query_as("INSERT INTO happyview_collections (nsid) VALUES (?) RETURNING id")
                .bind(&nsid)
                .fetch_one(pool)
                .await
                .expect("collection");
        let (did_id,): (i64,) =
            crate::db::query_as("INSERT INTO happyview_dids (did) VALUES (?) RETURNING id")
                .bind(format!("did:plc:z{}", uuid::Uuid::new_v4().simple()))
                .fetch_one(pool)
                .await
                .expect("did");
        let (dict_id,): (i64,) = crate::db::query_as(
            "INSERT INTO happyview_zstd_dicts (collection_id, created_at, dict) VALUES (?, 0, ?) RETURNING id",
        )
        .bind(collection_id)
        .bind(dict.to_vec())
        .fetch_one(pool)
        .await
        .expect("dictionary");
        (collection_id, did_id, dict_id)
    }

    #[test]
    fn compression_defaults_to_zstd() {
        assert_eq!(parse_compression(None), Compression::Zstd);
        assert_eq!(parse_compression(Some("zstd")), Compression::Zstd);
        assert_eq!(parse_compression(Some(" NONE ")), Compression::None);
        assert_eq!(parse_compression(Some("lz4")), Compression::Zstd);
    }

    #[test]
    fn the_codec_compresses_only_what_it_should() {
        let posts = sample_posts(1000);
        let dict = train_dict(&posts, 4096);
        let codec = BodyCodec::new(Compression::Zstd);
        assert_eq!(
            codec.encode(1, &posts[0]),
            (posts[0].as_bytes().to_vec(), None),
            "no dictionary yet"
        );
        codec.install(1, 7, &dict);
        assert_eq!(codec.current_dict(1), Some(7));
        assert_eq!(
            codec.encode(1, "{}"),
            (b"{}".to_vec(), Some(RAW_CONSIDERED)),
            "under 64 bytes"
        );
        let (body, dict_id) = codec.encode(1, &posts[3]);
        assert_eq!(dict_id, Some(7));
        assert!(
            body.len() < posts[3].len(),
            "{} >= {}",
            body.len(),
            posts[3].len()
        );
        let decoder = zstd::dict::DecoderDictionary::copy(&dict);
        assert_eq!(decompress(&body, &decoder).expect("decompress"), posts[3]);
        let off = BodyCodec::new(Compression::None);
        off.install(1, 7, &dict);
        assert_eq!(
            off.encode(1, &posts[3]).1,
            None,
            "RECORD_COMPRESSION=none stores raw"
        );
    }

    #[tokio::test]
    async fn raw_sql_reads_compressed_and_raw_bodies_byte_for_byte() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let posts = sample_posts(1000);
        let dict = train_dict(&posts, 4096);
        let (collection_id, did_id, dict_id) = seed(&pool, &dict).await;
        let codec = BodyCodec::new(Compression::Zstd);
        codec.install(collection_id, dict_id, &dict);
        let mut bodies = posts[..50].to_vec();
        bodies.push("{}".to_string());
        bodies.push(r#"{"z":1,"a":[1,2,{"k":"é\n"}],"via":"x"}"#.to_string());
        for (i, json) in bodies.iter().enumerate() {
            let (body, body_dict) = codec.encode(collection_id, json);
            crate::db::query(
                "INSERT INTO happyview_records_v2 (did_id, collection_id, rkey, cid, body, body_dict, created_at) \
                 VALUES (?, ?, ?, x'', ?, ?, 0)",
            )
            .bind(did_id)
            .bind(collection_id)
            .bind(format!("r{i}"))
            .bind(body)
            .bind(body_dict)
            .execute(&pool)
            .await
            .expect("insert");
        }
        let read: Vec<(String,)> = crate::db::query_as(
            "SELECT hv_record(body, body_dict) FROM happyview_records_v2 WHERE collection_id = ? ORDER BY id",
        )
        .bind(collection_id)
        .fetch_all(&pool)
        .await
        .expect("read back");
        assert_eq!(read.into_iter().map(|(s,)| s).collect::<Vec<_>>(), bodies);
        let via: Vec<(Option<String>,)> = crate::db::query_as(
            "SELECT json_extract(hv_record(body, body_dict), '$.via') FROM happyview_records_v2 \
             WHERE collection_id = ? AND body_dict = ? LIMIT 1",
        )
        .bind(collection_id)
        .bind(dict_id)
        .fetch_all(&pool)
        .await
        .expect("json_extract on a compressed body");
        assert_eq!(via, vec![(Some("witchsky".to_string()),)]);
    }

    /// Dictionary ids are per database. A process with two databases whose
    /// dictionary 1 differs decodes each with its own.
    #[tokio::test]
    async fn each_database_decodes_with_its_own_dictionaries() {
        let posts = sample_posts(1000);
        let other: Vec<String> = posts.iter().map(|p| p.replace("post", "skeet")).collect();
        for samples in [&posts, &other] {
            let pool = crate::test_support::migrated_memory_pool().await;
            let dict = train_dict(samples, 4096);
            let (collection_id, did_id, dict_id) = seed(&pool, &dict).await;
            let codec = BodyCodec::new(Compression::Zstd);
            codec.install(collection_id, dict_id, &dict);
            let (body, body_dict) = codec.encode(collection_id, &samples[5]);
            crate::db::query(
                "INSERT INTO happyview_records_v2 (did_id, collection_id, rkey, cid, body, body_dict, created_at) \
                 VALUES (?, ?, 'r', x'', ?, ?, 0)",
            )
            .bind(did_id)
            .bind(collection_id)
            .bind(body)
            .bind(body_dict)
            .execute(&pool)
            .await
            .expect("insert");
            let (text,): (String,) =
                crate::db::query_as("SELECT hv_record(body, body_dict) FROM happyview_records_v2")
                    .fetch_one(&pool)
                    .await
                    .expect("decode");
            assert_eq!(text, samples[5]);
        }
    }

    /// A body `decompress` would refuse is never compressed, or no read of
    /// it (or of its collection) could succeed.
    #[tokio::test]
    async fn a_body_past_the_decode_limit_stays_raw() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let posts = sample_posts(1000);
        let dict = train_dict(&posts, 4096);
        let (collection_id, did_id, dict_id) = seed(&pool, &dict).await;
        let codec = BodyCodec::new(Compression::Zstd);
        codec.install(collection_id, dict_id, &dict);
        let json = format!(r#"{{"text":"{}"}}"#, "a".repeat(MAX_RECORD_BYTES));
        assert!(json.len() > MAX_RECORD_BYTES);
        let (body, body_dict) = codec.encode(collection_id, &json);
        assert_eq!(body_dict, Some(RAW_CONSIDERED));
        assert!(body == json.as_bytes(), "an oversized body is stored raw");
        crate::db::query(
            "INSERT INTO happyview_records_v2 (did_id, collection_id, rkey, cid, body, body_dict, created_at) \
             VALUES (?, ?, 'r', x'', ?, ?, 0)",
        )
        .bind(did_id)
        .bind(collection_id)
        .bind(body)
        .bind(body_dict)
        .execute(&pool)
        .await
        .expect("insert");
        let (text,): (String,) =
            crate::db::query_as("SELECT hv_record(body, body_dict) FROM happyview_records_v2")
                .fetch_one(&pool)
                .await
                .expect("read an oversized body");
        assert!(text == json, "hv_record returns the oversized body intact");
    }

    #[tokio::test]
    async fn a_missing_dictionary_or_corrupt_frame_is_an_error() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let err = crate::db::query("SELECT hv_record(x'00', 7)")
            .execute(&pool)
            .await
            .expect_err("unknown dictionary");
        assert!(
            err.to_string().contains("zstd dictionary 7 does not exist"),
            "{err}"
        );
        let posts = sample_posts(1000);
        let (_, _, dict_id) = seed(&pool, &train_dict(&posts, 4096)).await;
        let err = crate::db::query("SELECT hv_record(x'28b52ffd00', ?)")
            .bind(dict_id)
            .execute(&pool)
            .await
            .expect_err("corrupt frame");
        assert!(
            err.to_string()
                .contains(&format!("hv_record: dictionary {dict_id}:")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn the_codec_loads_each_collections_newest_dictionary() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let posts = sample_posts(1000);
        let dict = train_dict(&posts, 4096);
        let (collection_id, _, first) = seed(&pool, &dict).await;
        let (second,): (i64,) = crate::db::query_as(
            "INSERT INTO happyview_zstd_dicts (collection_id, created_at, dict) VALUES (?, 1, ?) RETURNING id",
        )
        .bind(collection_id)
        .bind(dict.clone())
        .fetch_one(&pool)
        .await
        .expect("second dictionary");
        assert!(second > first);
        let codec = BodyCodec::load(&pool, Compression::Zstd)
            .await
            .expect("load");
        assert_eq!(codec.current_dict(collection_id), Some(second));
        let off = BodyCodec::load(&pool, Compression::None)
            .await
            .expect("load");
        assert_eq!(off.current_dict(collection_id), None);
    }

    /// Seed `bodies` as raw, never-considered rows of a fresh collection.
    async fn seed_raw(pool: &sqlx::AnyPool, bodies: &[String]) -> i64 {
        let (collection_id,): (i64,) =
            crate::db::query_as("INSERT INTO happyview_collections (nsid) VALUES (?) RETURNING id")
                .bind(format!("app.test.train{}", uuid::Uuid::new_v4().simple()))
                .fetch_one(pool)
                .await
                .expect("collection");
        let (did_id,): (i64,) =
            crate::db::query_as("INSERT INTO happyview_dids (did) VALUES (?) RETURNING id")
                .bind(format!("did:plc:t{}", uuid::Uuid::new_v4().simple()))
                .fetch_one(pool)
                .await
                .expect("did");
        for (i, body) in bodies.iter().enumerate() {
            crate::db::query(
                "INSERT INTO happyview_records_v2 (did_id, collection_id, rkey, cid, body, created_at) \
                 VALUES (?, ?, ?, x'', CAST(? AS BLOB), 0)",
            )
            .bind(did_id)
            .bind(collection_id)
            .bind(format!("r{i}"))
            .bind(body.as_str())
            .execute(pool)
            .await
            .expect("seed row");
        }
        collection_id
    }

    #[test]
    fn training_and_interval_settings_parse_with_defaults() {
        assert_eq!(parse_train_at(None), DEFAULT_TRAIN_AT);
        assert_eq!(parse_train_at(Some("200")), 200);
        assert_eq!(parse_train_at(Some("0")), DEFAULT_TRAIN_AT);
        assert_eq!(parse_train_at(Some("lots")), DEFAULT_TRAIN_AT);
        assert_eq!(parse_interval_secs(None), DEFAULT_INTERVAL_SECS);
        assert_eq!(parse_interval_secs(Some("5")), 5);
        assert_eq!(parse_interval_secs(Some("0")), DEFAULT_INTERVAL_SECS);
    }

    #[tokio::test]
    async fn a_collection_past_the_threshold_trains_and_its_raw_rows_recompress() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let mut bodies = sample_posts(1200);
        bodies.push("{}".to_string());
        let collection_id = seed_raw(&pool, &bodies).await;
        let small = seed_raw(&pool, &sample_posts(10)).await;

        let due = due_for_training(&pool, 1000).await.expect("due");
        assert!(
            due.contains(&collection_id) && !due.contains(&small),
            "{due:?}"
        );

        let codec = BodyCodec::new(Compression::Zstd);
        let dict_id = train(&pool, &codec, collection_id, 4096)
            .await
            .expect("train");
        assert_eq!(codec.current_dict(collection_id), Some(dict_id));
        assert!(
            due_for_training(&pool, 1000).await.expect("due").is_empty(),
            "a trained collection is no longer due"
        );

        let mut considered = 0;
        loop {
            let batch =
                recompress_batch(&pool, &codec, collection_id, 500, RECOMPRESS_BUDGET_BYTES)
                    .await
                    .expect("recompress");
            considered += batch.considered;
            if !batch.more {
                break;
            }
        }
        assert_eq!(considered as usize, bodies.len());
        let (left,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM happyview_records_v2 WHERE collection_id = ? AND body_dict IS NULL",
        )
        .bind(collection_id)
        .fetch_one(&pool)
        .await
        .expect("count");
        assert_eq!(left, 0);
        let (stored, raw): (i64, i64) = crate::db::query_as(
            "SELECT SUM(length(body)), SUM(length(hv_record(body, body_dict))) FROM happyview_records_v2 WHERE collection_id = ?",
        )
        .bind(collection_id)
        .fetch_one(&pool)
        .await
        .expect("sizes");
        assert!(stored * 2 < raw, "compressed {stored} of {raw} bytes");
        let read: Vec<(String,)> = crate::db::query_as(
            "SELECT hv_record(body, body_dict) FROM happyview_records_v2 WHERE collection_id = ? ORDER BY id",
        )
        .bind(collection_id)
        .fetch_all(&pool)
        .await
        .expect("read back");
        assert_eq!(read.into_iter().map(|(s,)| s).collect::<Vec<_>>(), bodies);
        let (tiny,): (i64,) = crate::db::query_as(
            "SELECT body_dict FROM happyview_records_v2 WHERE collection_id = ? AND body = CAST('{}' AS BLOB)",
        )
        .bind(collection_id)
        .fetch_one(&pool)
        .await
        .expect("the tiny row");
        assert_eq!(tiny, RAW_CONSIDERED);
    }

    #[tokio::test]
    async fn the_loop_does_nothing_when_compression_is_off_or_on_postgres() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let off = crate::record_store::RecordStore::with_bodies(
            pool.clone(),
            DatabaseBackend::Sqlite,
            BodyCodec::disabled(),
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run(off, 1, std::time::Duration::from_secs(3600)),
        )
        .await
        .expect("returns at once when compression is off");
        let pg = crate::record_store::RecordStore::with_bodies(
            pool,
            DatabaseBackend::Postgres,
            BodyCodec::new(Compression::Zstd),
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run(pg, 1, std::time::Duration::from_secs(3600)),
        )
        .await
        .expect("returns at once on Postgres");
    }

    #[test]
    fn only_a_zstd_dictionary_passes_the_magic_check() {
        let trained = train_dict(&sample_posts(1000), 4096);
        assert!(is_zstd_dictionary(&trained));
        assert!(!is_zstd_dictionary(b"{\"text\":\"raw content\"}"));
        assert!(!is_zstd_dictionary(&[0x37, 0xA4, 0x30]));
        assert!(!is_zstd_dictionary(&[]));
    }

    async fn raw_left(pool: &sqlx::AnyPool, collection_id: i64) -> i64 {
        let (left,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM happyview_records_v2 WHERE collection_id = ? AND body_dict IS NULL",
        )
        .bind(collection_id)
        .fetch_one(pool)
        .await
        .expect("count raw rows");
        left
    }

    #[tokio::test]
    async fn training_samples_skip_oversized_bodies_and_stop_at_the_byte_budget() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let posts = sample_posts(3);
        let huge = format!(r#"{{"text":"{}"}}"#, "a".repeat(2000));
        let bodies = vec![posts[0].clone(), huge, posts[1].clone(), posts[2].clone()];
        let collection_id = seed_raw(&pool, &bodies).await;

        let all = training_samples(&pool, collection_id, 1000, usize::MAX)
            .await
            .expect("samples");
        assert_eq!(
            all,
            vec![posts[2].clone(), posts[1].clone(), posts[0].clone()],
            "newest first, the body over the per-sample cap skipped"
        );
        let budget = posts[2].len() + posts[1].len();
        let capped = training_samples(&pool, collection_id, 1000, budget)
            .await
            .expect("samples");
        assert_eq!(capped, vec![posts[2].clone(), posts[1].clone()]);
    }

    #[tokio::test]
    async fn a_byte_capped_batch_ends_early_and_the_loop_still_finishes() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let bodies = sample_posts(100);
        let collection_id = seed_raw(&pool, &bodies).await;
        let codec = BodyCodec::new(Compression::Zstd);
        let dict = train_dict(&sample_posts(1000), 4096);
        let (dict_id,): (i64,) = crate::db::query_as(
            "INSERT INTO happyview_zstd_dicts (collection_id, created_at, dict) VALUES (?, 0, ?) RETURNING id",
        )
        .bind(collection_id)
        .bind(dict.clone())
        .fetch_one(&pool)
        .await
        .expect("dictionary");
        codec.install(collection_id, dict_id, &dict);

        let one = recompress_batch(&pool, &codec, collection_id, 1000, 1)
            .await
            .expect("recompress");
        assert_eq!(
            (one.considered, one.more),
            (1, true),
            "a batch always makes progress"
        );

        // The next three bodies fill the budget exactly.
        let budget: usize = bodies[1..4].iter().map(String::len).sum();
        let three = recompress_batch(&pool, &codec, collection_id, 1000, budget)
            .await
            .expect("recompress");
        assert_eq!(
            (three.considered, three.more),
            (3, true),
            "the byte cap ends the batch"
        );

        // A batch passes its budget by at most the row that reaches it.
        let shortest = bodies.iter().map(String::len).min().unwrap_or(1);
        let most = (budget / shortest + 1) as u64;
        let mut considered = one.considered + three.considered;
        let mut batches = 0;
        loop {
            let batch = recompress_batch(&pool, &codec, collection_id, 1000, budget)
                .await
                .expect("recompress");
            assert!(
                batch.considered <= most,
                "{} rows past the byte budget",
                batch.considered
            );
            considered += batch.considered;
            batches += 1;
            if !batch.more {
                break;
            }
        }
        assert!(batches > 1);
        assert_eq!(considered as usize, bodies.len());
        assert_eq!(raw_left(&pool, collection_id).await, 0);
    }

    #[tokio::test]
    async fn a_pass_trains_due_collections_and_recompresses_past_a_failing_one() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let store = crate::record_store::RecordStore::with_bodies(
            pool.clone(),
            DatabaseBackend::Sqlite,
            BodyCodec::new(Compression::Zstd),
        );
        // A collection with a dictionary whose one raw row was written as
        // TEXT behind the store's back, so recompressing it fails.
        let broken = seed_raw(&pool, &[]).await;
        let dict = train_dict(&sample_posts(1000), 4096);
        let (dict_id,): (i64,) = crate::db::query_as(
            "INSERT INTO happyview_zstd_dicts (collection_id, created_at, dict) VALUES (?, 0, ?) RETURNING id",
        )
        .bind(broken)
        .bind(dict.clone())
        .fetch_one(&pool)
        .await
        .expect("dictionary");
        store.bodies().install(broken, dict_id, &dict);
        let (did_id,): (i64,) = crate::db::query_as(
            "INSERT INTO happyview_dids (did) VALUES ('did:plc:text') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .expect("did");
        crate::db::query(
            "INSERT INTO happyview_records_v2 (did_id, collection_id, rkey, cid, body, created_at) \
             VALUES (?, ?, 'text', x'', ?, 0)",
        )
        .bind(did_id)
        .bind(broken)
        .bind(sample_posts(1)[0].as_str())
        .execute(&pool)
        .await
        .expect("text row");
        assert!(
            recompress_batch(&pool, store.bodies(), broken, 10, RECOMPRESS_BUDGET_BYTES)
                .await
                .is_err(),
            "a TEXT body fails its batch"
        );

        let bodies = sample_posts(1200);
        let due = seed_raw(&pool, &bodies).await;
        assert!(due > broken);
        let considered = pass(&store, 1000).await.expect("pass");
        assert_eq!(considered as usize, bodies.len());
        let dict = store
            .bodies()
            .current_dict(due)
            .expect("trained in the pass");
        let (stored,): (i64,) =
            crate::db::query_as("SELECT MAX(id) FROM happyview_zstd_dicts WHERE collection_id = ?")
                .bind(due)
                .fetch_one(&pool)
                .await
                .expect("stored dictionary");
        assert_eq!(dict, stored);
        assert_eq!(raw_left(&pool, due).await, 0);
    }
}
