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

use sqlx::AnyPool;

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
    /// newest dictionary when that helps, raw otherwise.
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
        if raw.len() < MIN_COMPRESS_BYTES {
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

    fn train(samples: &[String], size: usize) -> Vec<u8> {
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
        let dict = train(&posts, 4096);
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
        let dict = train(&posts, 4096);
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
            let dict = train(samples, 4096);
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
        let (_, _, dict_id) = seed(&pool, &train(&posts, 4096)).await;
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
        let dict = train(&posts, 4096);
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
}
