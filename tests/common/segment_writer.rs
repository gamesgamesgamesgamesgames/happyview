//! Writes Jetstream segment files for the mock archive. HappyView's reader is
//! pinned to the upstream golden segment (Task 2/3); this writer is pinned to
//! the reader by `the_test_writer_round_trips_through_the_reader`.

#[derive(Clone, Debug)]
pub struct ArchiveEvent {
    pub seq: u64,
    pub witnessed_at: i64,
    pub kind: u8,
    pub did: String,
    pub collection: String,
    pub rkey: String,
    pub rev: String,
    pub payload: Vec<u8>,
}

pub struct WrittenSegment {
    pub bytes: Vec<u8>,
    /// 16 hex digits, the ETag and plan checksum. Not Jetstream's xxh3 (the
    /// reader does not check it); unique per content.
    pub checksum: String,
    /// Per block: min seq, max seq, offset of its length prefix, frame size.
    pub blocks: Vec<(u64, u64, u64, u32)>,
}

fn encode_block(events: &[ArchiveEvent]) -> Vec<u8> {
    let mut out = (events.len() as u32).to_le_bytes().to_vec();
    for e in events {
        out.extend(e.seq.to_le_bytes());
    }
    for e in events {
        out.extend(e.witnessed_at.to_le_bytes());
    }
    for _ in events {
        out.extend(0i64.to_le_bytes());
    }
    for e in events {
        out.push(e.kind);
    }
    for e in events {
        out.push(e.collection.len() as u8);
    }
    for e in events {
        out.extend((e.did.len() as u16).to_le_bytes());
    }
    for e in events {
        out.push(e.rkey.len() as u8);
    }
    for e in events {
        out.push(e.rev.len() as u8);
    }
    for e in events {
        out.extend((e.payload.len() as u32).to_le_bytes());
    }
    for e in events {
        out.extend(e.collection.as_bytes());
    }
    for e in events {
        out.extend(e.did.as_bytes());
    }
    for e in events {
        out.extend(e.rkey.as_bytes());
    }
    for e in events {
        out.extend(e.rev.as_bytes());
    }
    for e in events {
        out.extend(&e.payload);
    }
    out
}

/// `events` (seq order) into a sealed segment, `per_block` events a block.
pub fn write_segment(events: &[ArchiveEvent], per_block: usize) -> WrittenSegment {
    assert!(!events.is_empty() && per_block > 0);
    let mut body = Vec::new();
    let mut blocks = Vec::new();
    let mut index = Vec::new();
    for chunk in events.chunks(per_block) {
        let decoded = encode_block(chunk);
        let frame = zstd::bulk::compress(&decoded, 3).expect("compress");
        let offset = 256 + body.len() as u64;
        body.extend((frame.len() as u64).to_le_bytes());
        body.extend(&frame);
        let (min, max) = (chunk[0].seq, chunk[chunk.len() - 1].seq);
        blocks.push((min, max, offset, frame.len() as u32));
        index.extend(offset.to_le_bytes());
        index.extend((frame.len() as u32).to_le_bytes());
        index.extend((decoded.len() as u32).to_le_bytes());
        index.extend((chunk.len() as u32).to_le_bytes());
        index.extend(min.to_le_bytes());
        index.extend(max.to_le_bytes());
        index.extend(chunk[0].witnessed_at.to_le_bytes());
        index.extend(chunk[chunk.len() - 1].witnessed_at.to_le_bytes());
    }
    let footer_offset = 256 + body.len() as u64;
    let mut header = vec![0u8; 256];
    header[0..4].copy_from_slice(b"jss0");
    header[12..14].copy_from_slice(&1u16.to_le_bytes());
    header[14..18].copy_from_slice(&(blocks.len() as u32).to_le_bytes());
    header[18..22].copy_from_slice(&(events.len() as u32).to_le_bytes());
    header[26..34].copy_from_slice(&events[0].seq.to_le_bytes());
    header[34..42].copy_from_slice(&events[events.len() - 1].seq.to_le_bytes());
    header[42..50].copy_from_slice(&events[0].witnessed_at.to_le_bytes());
    header[50..58].copy_from_slice(&events[events.len() - 1].witnessed_at.to_le_bytes());
    header[58..66].copy_from_slice(&footer_offset.to_le_bytes());
    let end = footer_offset + index.len() as u64;
    for at in [66usize, 74, 82] {
        // Blooms and the collection index are absent: point them at the end.
        header[at..at + 8].copy_from_slice(&end.to_le_bytes());
    }
    header[90..98].copy_from_slice(&footer_offset.to_le_bytes());
    let mut bytes = header;
    bytes.extend(body);
    bytes.extend(index);
    let digest = <sha2::Sha256 as sha2::Digest>::digest(&bytes);
    let checksum: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    bytes[4..12].copy_from_slice(&u64::from_str_radix(&checksum, 16).unwrap().to_le_bytes());
    WrittenSegment {
        bytes,
        checksum,
        blocks,
    }
}
