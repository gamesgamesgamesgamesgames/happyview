//! A minimal reader for Jetstream's sealed segment files (`.jss`), enough to
//! decode the blocks a replay plan names. Blooms and the collection index are
//! not read: the server prunes with them, and the client filters exactly.
//! Format: Jetstream `docs/README.md` §2.4, as summarised in the Phase 2
//! protocol digest.

use std::io::{Read, Seek, SeekFrom};

pub const HEADER_LEN: u64 = 256;
pub const INDEX_ENTRY_LEN: u64 = 52;
/// Decoded block cap. The protocol allows 1 GiB; real blocks are a few MiB
/// (4,096 events), and a bigger one is recorded as a failed block (P5).
pub const MAX_DECODED_BLOCK: usize = 64 << 20;
pub const MAX_BLOCK_EVENTS: usize = 262_144;
const MAGIC: &[u8; 4] = b"jss0";
const VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentError(pub String);

impl std::fmt::Display for SegmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SegmentError {}

fn err(message: impl Into<String>) -> SegmentError {
    SegmentError(message.into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockEntry {
    /// Absolute offset of the block's 8-byte length prefix.
    pub offset: u64,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub event_count: u32,
    pub min_seq: u64,
    pub max_seq: u64,
}

/// One archived event, as stored. `kind`: 1 create, 2 update, 3 delete,
/// 4 identity, 5 account, 6 sync, 7 resync create; anything else is a kind
/// this reader predates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub seq: u64,
    pub witnessed_at: i64,
    pub indexed_at: i64,
    pub kind: u8,
    pub collection: String,
    pub did: String,
    pub rkey: String,
    pub rev: String,
    pub payload: Vec<u8>,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().expect("2 bytes"))
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// Read the header and the block index, checking every entry lies inside the
/// block region `[256, footer_offset)`.
pub fn read_index<R: Read + Seek>(
    file: &mut R,
    file_len: u64,
) -> Result<Vec<BlockEntry>, SegmentError> {
    if file_len < HEADER_LEN {
        return Err(err("the segment is shorter than its header"));
    }
    let mut header = [0u8; HEADER_LEN as usize];
    file.seek(SeekFrom::Start(0))
        .map_err(|e| err(e.to_string()))?;
    file.read_exact(&mut header)
        .map_err(|e| err(e.to_string()))?;
    if &header[0..4] != MAGIC {
        return Err(err("not a jetstream segment (bad magic)"));
    }
    let version = u16_at(&header, 12);
    if version != VERSION {
        return Err(err(format!("unsupported segment version {version}")));
    }
    let block_count = u32_at(&header, 14) as u64;
    let footer_offset = u64_at(&header, 58);
    let index_offset = u64_at(&header, 90);
    if footer_offset < HEADER_LEN || footer_offset > file_len {
        return Err(err(format!(
            "footer offset {footer_offset} is outside the file"
        )));
    }
    let index_len = block_count
        .checked_mul(INDEX_ENTRY_LEN)
        .ok_or_else(|| err("block count overflows"))?;
    if index_offset < footer_offset || index_offset.saturating_add(index_len) > file_len {
        return Err(err("the block index is outside the footer"));
    }
    let mut index = vec![0u8; index_len as usize];
    file.seek(SeekFrom::Start(index_offset))
        .map_err(|e| err(e.to_string()))?;
    file.read_exact(&mut index)
        .map_err(|e| err(e.to_string()))?;
    let mut entries = Vec::with_capacity(block_count as usize);
    for i in 0..block_count as usize {
        let at = i * INDEX_ENTRY_LEN as usize;
        let entry = BlockEntry {
            offset: u64_at(&index, at),
            compressed_size: u32_at(&index, at + 8),
            uncompressed_size: u32_at(&index, at + 12),
            event_count: u32_at(&index, at + 16),
            min_seq: u64_at(&index, at + 20),
            max_seq: u64_at(&index, at + 28),
        };
        let end = entry
            .offset
            .checked_add(8 + entry.compressed_size as u64)
            .ok_or_else(|| err("block offset overflows"))?;
        if entry.offset < HEADER_LEN || end > footer_offset {
            return Err(err(format!("block {i} lies outside the block region")));
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// The zstd frame of `entry`, without its length prefix.
pub fn read_block_frame<R: Read + Seek>(
    file: &mut R,
    entry: &BlockEntry,
) -> Result<Vec<u8>, SegmentError> {
    file.seek(SeekFrom::Start(entry.offset))
        .map_err(|e| err(e.to_string()))?;
    let mut len = [0u8; 8];
    file.read_exact(&mut len).map_err(|e| err(e.to_string()))?;
    let len = u64::from_le_bytes(len);
    if len != entry.compressed_size as u64 {
        return Err(err(format!(
            "block length prefix {len} disagrees with the index ({})",
            entry.compressed_size
        )));
    }
    let mut frame = vec![0u8; len as usize];
    file.read_exact(&mut frame)
        .map_err(|e| err(e.to_string()))?;
    Ok(frame)
}

/// Decompress and parse one block (a `getBlock` body, or a frame read from a
/// segment file).
pub fn decode_block(frame: &[u8]) -> Result<Vec<Row>, SegmentError> {
    let block = decompress_capped(frame)?;
    let mut at = 0usize;
    let take = |at: &mut usize, n: usize| -> Result<std::ops::Range<usize>, SegmentError> {
        let end = at.checked_add(n).filter(|end| *end <= block.len());
        let Some(end) = end else {
            return Err(err("block is shorter than its columns"));
        };
        let range = *at..end;
        *at = end;
        Ok(range)
    };
    let n = u32_at(&block[take(&mut at, 4)?], 0) as usize;
    if n > MAX_BLOCK_EVENTS {
        return Err(err(format!("block claims {n} events")));
    }
    let seqs = take(&mut at, 8 * n)?;
    let witnessed = take(&mut at, 8 * n)?;
    let indexed = take(&mut at, 8 * n)?;
    let kinds = take(&mut at, n)?;
    let collection_lens = take(&mut at, n)?;
    let did_lens = take(&mut at, 2 * n)?;
    let rkey_lens = take(&mut at, n)?;
    let rev_lens = take(&mut at, n)?;
    let payload_lens = take(&mut at, 4 * n)?;

    let text = |range: std::ops::Range<usize>| {
        std::str::from_utf8(&block[range])
            .map(str::to_string)
            .map_err(|_| err("identifier is not UTF-8"))
    };
    let mut collections = Vec::with_capacity(n);
    for i in 0..n {
        let r = take(&mut at, block[collection_lens.start + i] as usize)?;
        collections.push(text(r)?);
    }
    let mut dids = Vec::with_capacity(n);
    for i in 0..n {
        let r = take(&mut at, u16_at(&block, did_lens.start + 2 * i) as usize)?;
        dids.push(text(r)?);
    }
    let mut rkeys = Vec::with_capacity(n);
    for i in 0..n {
        let r = take(&mut at, block[rkey_lens.start + i] as usize)?;
        rkeys.push(text(r)?);
    }
    let mut revs = Vec::with_capacity(n);
    for i in 0..n {
        let r = take(&mut at, block[rev_lens.start + i] as usize)?;
        revs.push(text(r)?);
    }
    let mut rows = Vec::with_capacity(n);
    for (i, ((collection, did), (rkey, rev))) in collections
        .into_iter()
        .zip(dids)
        .zip(rkeys.into_iter().zip(revs))
        .enumerate()
    {
        let payload = take(&mut at, u32_at(&block, payload_lens.start + 4 * i) as usize)?;
        rows.push(Row {
            seq: u64_at(&block, seqs.start + 8 * i),
            witnessed_at: u64_at(&block, witnessed.start + 8 * i) as i64,
            indexed_at: u64_at(&block, indexed.start + 8 * i) as i64,
            kind: block[kinds.start + i],
            collection,
            did,
            rkey,
            rev,
            payload: block[payload].to_vec(),
        });
    }
    if at != block.len() {
        return Err(err(format!(
            "{} trailing bytes after the block",
            block.len() - at
        )));
    }
    Ok(rows)
}

/// Decompress one zstd frame, refusing anything over `MAX_DECODED_BLOCK`.
/// `zstd::bulk::decompress(frame, cap)` would reserve the whole cap for every
/// block, 64 MiB each even for a few KiB, times `BACKFILL_REPLAY_CONCURRENCY`.
fn decompress_capped(frame: &[u8]) -> Result<Vec<u8>, SegmentError> {
    let block = read_capped(frame, MAX_DECODED_BLOCK)?;
    if block.len() > MAX_DECODED_BLOCK {
        return Err(err(format!(
            "block decompresses to more than {MAX_DECODED_BLOCK} bytes"
        )));
    }
    Ok(block)
}

/// Decode `frame` in fixed chunks, stopping one byte past `cap`. The buffer
/// doubles but is clamped to `cap + 1`, so the capacity never exceeds it
/// (a plain doubling `read_to_end` would reach twice that before refusing).
fn read_capped(frame: &[u8], cap: usize) -> Result<Vec<u8>, SegmentError> {
    const CHUNK: usize = 64 << 10;
    let mut decoder = zstd::stream::read::Decoder::with_buffer(frame)
        .map_err(|e| err(format!("block is not a zstd frame: {e}")))?;
    let limit = cap + 1;
    let mut block: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; CHUNK];
    while block.len() < limit {
        let want = CHUNK.min(limit - block.len());
        let read = decoder
            .read(&mut chunk[..want])
            .map_err(|e| err(format!("block does not decompress: {e}")))?;
        if read == 0 {
            break;
        }
        if block.capacity() - block.len() < read {
            // Double, but never past the limit.
            let step = block.len().max(CHUNK).min(limit - block.len());
            block.reserve_exact(step);
        }
        block.extend_from_slice(&chunk[..read]);
    }
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn golden() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/jetstream/corpus/golden_corpus_segment.jss"
        ))
        .expect("golden segment")
    }

    fn golden_keys() -> Vec<(String, String)> {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/jetstream/corpus/car_expected_records.tsv"
        ))
        .unwrap()
        .lines()
        .map(|line| {
            let key = line.split('\t').next().unwrap();
            let (collection, rkey) = key.split_once('/').unwrap();
            (collection.to_string(), rkey.to_string())
        })
        .collect()
    }

    fn all_rows(bytes: &[u8]) -> Vec<Row> {
        let mut file = Cursor::new(bytes);
        let index = read_index(&mut file, bytes.len() as u64).expect("index");
        let mut rows = Vec::new();
        for entry in &index {
            let frame = read_block_frame(&mut file, entry).expect("frame");
            rows.extend(decode_block(&frame).expect("block"));
        }
        rows
    }

    #[test]
    fn the_golden_segment_reads_as_jetstream_wrote_it() {
        let bytes = golden();
        let index = read_index(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
        assert_eq!(index.len(), 3);
        assert_eq!(
            index
                .iter()
                .map(|e| (e.event_count, e.min_seq, e.max_seq))
                .collect::<Vec<_>>(),
            vec![(8, 1, 8), (8, 9, 16), (7, 17, 23)]
        );
        assert_eq!(index[0].offset, 256);

        let rows = all_rows(&bytes);
        let keys = golden_keys();
        assert_eq!(rows.len(), 23);
        for (i, (row, (collection, rkey))) in rows.iter().zip(&keys).enumerate() {
            let seq = i as u64 + 1;
            assert_eq!(row.seq, seq);
            assert_eq!(row.witnessed_at, 1_750_000_000_000_000 + seq as i64);
            assert_eq!(row.indexed_at, 0);
            assert_eq!(row.kind, 1, "KindCreate");
            assert_eq!(row.did, "did:plc:53iucmf5ldftgqyf2ac7hmhd");
            assert_eq!(row.rev, "3migo7fijwi2k");
            assert_eq!((&row.collection, &row.rkey), (collection, rkey));
            assert!(!row.payload.is_empty());
        }
    }

    #[test]
    fn a_wrong_magic_or_version_is_refused() {
        let mut bytes = golden();
        bytes[0] = b'x';
        assert!(read_index(&mut Cursor::new(&bytes), bytes.len() as u64).is_err());
        let mut bytes = golden();
        bytes[12] = 2;
        assert!(read_index(&mut Cursor::new(&bytes), bytes.len() as u64).is_err());
    }

    #[test]
    fn index_entries_outside_the_block_region_are_refused() {
        let mut bytes = golden();
        let index = u64::from_le_bytes(bytes[90..98].try_into().unwrap()) as usize;
        // Point block 0 into the header.
        bytes[index..index + 8].copy_from_slice(&100u64.to_le_bytes());
        assert!(read_index(&mut Cursor::new(&bytes), bytes.len() as u64).is_err());
        // A truncated file cannot hold its own index.
        let bytes = golden();
        assert!(read_index(&mut Cursor::new(&bytes[..1000]), 1000).is_err());
    }

    #[test]
    fn a_length_prefix_that_disagrees_with_the_index_is_refused() {
        let mut bytes = golden();
        bytes[256..264].copy_from_slice(&1u64.to_le_bytes());
        let index = read_index(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
        assert!(read_block_frame(&mut Cursor::new(&bytes), &index[0]).is_err());
    }

    /// Build a decoded block by hand, so malformed shapes can be encoded.
    type TestRow<'a> = (u64, u8, &'a str, &'a str, &'a str, &'a str, &'a [u8]);

    fn block(rows: &[TestRow]) -> Vec<u8> {
        let mut out = (rows.len() as u32).to_le_bytes().to_vec();
        for r in rows {
            out.extend(r.0.to_le_bytes());
        }
        for r in rows {
            out.extend((1_000 + r.0 as i64).to_le_bytes());
        }
        for _ in rows {
            out.extend(0i64.to_le_bytes());
        }
        for r in rows {
            out.push(r.1);
        }
        for r in rows {
            out.push(r.2.len() as u8);
        }
        for r in rows {
            out.extend((r.3.len() as u16).to_le_bytes());
        }
        for r in rows {
            out.push(r.4.len() as u8);
        }
        for r in rows {
            out.push(r.5.len() as u8);
        }
        for r in rows {
            out.extend((r.6.len() as u32).to_le_bytes());
        }
        for r in rows {
            out.extend(r.2.as_bytes());
        }
        for r in rows {
            out.extend(r.3.as_bytes());
        }
        for r in rows {
            out.extend(r.4.as_bytes());
        }
        for r in rows {
            out.extend(r.5.as_bytes());
        }
        for r in rows {
            out.extend(r.6);
        }
        out
    }

    fn frame(decoded: &[u8]) -> Vec<u8> {
        zstd::bulk::compress(decoded, 3).unwrap()
    }

    #[test]
    fn an_empty_block_has_no_rows() {
        assert_eq!(decode_block(&frame(&0u32.to_le_bytes())).unwrap(), vec![]);
    }

    #[test]
    fn markers_and_deletes_carry_what_they_have() {
        let rows = decode_block(&frame(&block(&[
            (5, 3, "app.test.post", "did:plc:a", "r1", "rev1", b""),
            (6, 5, "", "did:plc:b", "", "", b"\xa0"),
            (7, 99, "", "did:plc:c", "", "", b""),
        ])))
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[0].kind, rows[0].payload.len()), (3, 0));
        assert_eq!(rows[1].did, "did:plc:b");
        // Unknown kinds are kept here; the conversion (Task 4) skips them.
        assert_eq!(rows[2].kind, 99);
        assert_eq!(rows[2].witnessed_at, 1_007);
    }

    #[test]
    fn trailing_bytes_and_short_columns_are_refused() {
        let mut decoded = block(&[(1, 1, "a.b.c", "did:plc:a", "r", "v", b"\xa0")]);
        decoded.push(0);
        assert!(decode_block(&frame(&decoded)).is_err(), "trailing byte");
        let decoded = block(&[(1, 1, "a.b.c", "did:plc:a", "r", "v", b"\xa0")]);
        assert!(
            decode_block(&frame(&decoded[..decoded.len() - 1])).is_err(),
            "short payload"
        );
        assert!(decode_block(&frame(&[1, 0])).is_err(), "short count");
    }

    #[test]
    fn invalid_utf8_identifiers_are_refused() {
        let mut decoded = block(&[(1, 1, "a.b.c", "did:plc:a", "r", "v", b"")]);
        // The strings follow the columns in order: collection, DID, rkey,
        // rev, payload. With an empty payload, the DID ends 2 bytes before
        // the end ("r", "v"), so it starts at len - 2 - 9.
        let at = decoded.len() - 2 - "did:plc:a".len();
        assert_eq!(
            &decoded[at..at + 9],
            b"did:plc:a",
            "the offset lands on the DID"
        );
        decoded[at] = 0xff;
        assert!(decode_block(&frame(&decoded)).is_err());
    }

    #[test]
    fn decoding_never_reserves_more_than_the_cap_plus_one() {
        let over = read_capped(&frame(&vec![0u8; MAX_DECODED_BLOCK + 1]), MAX_DECODED_BLOCK)
            .expect("reads up to the limit");
        assert_eq!(over.len(), MAX_DECODED_BLOCK + 1);
        assert!(
            over.capacity() <= MAX_DECODED_BLOCK + 1,
            "capacity {}",
            over.capacity()
        );
        assert!(decode_block(&frame(&vec![0u8; MAX_DECODED_BLOCK + 1])).is_err());

        let small = read_capped(&frame(&[7u8; 1000]), MAX_DECODED_BLOCK).unwrap();
        assert_eq!(small.len(), 1000);
        assert!(
            small.capacity() <= 64 << 10,
            "capacity {}",
            small.capacity()
        );
    }

    #[test]
    fn an_absurd_event_count_is_refused_before_reading_columns() {
        let decoded = ((MAX_BLOCK_EVENTS + 1) as u32).to_le_bytes().to_vec();
        assert!(decode_block(&frame(&decoded)).is_err());
    }
}
