//! Reading and writing STAR archives.
//!
//! A STAR archive is a metadata header followed by key/value pairs, each pair
//! preceded by a `0x01` control byte, and a `0x00` control byte ending the
//! archive:
//!
//! ```text
//! [ magic | len | meta ] [ 1 | len | key | len | val ] ... [ 0 ]
//! ```
//!
//! Each `len` is an unsigned varint in its shortest form. The metadata is a
//! canonical DRISL map with a string `$type`, which names the profile that
//! gives the keys and values their meaning. The format itself is ordered but
//! not sorted, and allows duplicate keys; a profile that needs either property
//! checks it itself.
//!
//! The framing follows `@atproto/star` and is pinned by the upstream interop
//! fixtures in `tests/interop/`. Proposal 0016's text shows the archive without
//! control bytes; the reference implementation and the fixtures have them.
//!
//! See <https://dasl.ing/star.html>.

use std::collections::BTreeMap;
use std::io::Read;

use ipld_core::ipld::Ipld;

/// The four bytes every archive starts with.
pub const MAGIC: [u8; 4] = [0x2a, 0x6c, 0x01, 0x00];

const CONTROL_PAIR: u8 = 0x01;
const CONTROL_END: u8 = 0x00;

/// A `u64` needs at most ten 7-bit groups, but STAR caps a length at nine
/// bytes, so a longer prefix is refused before it can describe an absurd size.
const MAX_VARINT_BYTES: usize = 9;

/// Errors reading or writing an archive.
#[derive(Debug, thiserror::Error)]
pub enum StarError {
    #[error("reading the archive failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid magic bytes")]
    InvalidMagic,
    #[error("unexpected end of archive")]
    UnexpectedEnd,
    #[error("non-canonical varint")]
    NonCanonicalVarint,
    #[error("varint too long")]
    VarintTooLong,
    #[error("{what} length {length} exceeds the limit of {limit}")]
    TooLong {
        what: &'static str,
        length: u64,
        limit: u64,
    },
    #[error("invalid metadata: {0}")]
    InvalidMeta(String),
    #[error("metadata is not canonical DRISL")]
    NonCanonicalMeta,
    #[error("invalid control byte: {0:#04x}")]
    InvalidControlByte(u8),
    #[error("unexpected bytes after the end of the archive")]
    TrailingBytes,
}

/// Length limits for reading. STAR sets none of its own, so each is unlimited
/// unless a profile sets it.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_meta: u64,
    pub max_key: u64,
    pub max_value: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_meta: u64::MAX,
            max_key: u64::MAX,
            max_value: u64::MAX,
        }
    }
}

/// One key/value pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

/// Encode the header: the magic bytes and the metadata.
///
/// `meta` must be a map with a string `$type`.
pub fn encode_header(meta: &BTreeMap<String, Ipld>) -> Result<Vec<u8>, StarError> {
    match meta.get("$type") {
        Some(Ipld::String(_)) => {}
        _ => return Err(StarError::InvalidMeta("missing a string $type".into())),
    }
    let encoded = serde_ipld_dagcbor::to_vec(&Ipld::Map(meta.clone()))
        .map_err(|e| StarError::InvalidMeta(e.to_string()))?;
    let mut out = MAGIC.to_vec();
    frame(&mut out, &encoded);
    Ok(out)
}

/// Encode one pair, with its control byte.
pub fn encode_pair(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut out = vec![CONTROL_PAIR];
    frame(&mut out, key);
    frame(&mut out, value);
    out
}

/// The byte that ends an archive.
pub const END: [u8; 1] = [CONTROL_END];

/// Encode a whole archive in memory.
pub fn write_star<'a>(
    meta: &BTreeMap<String, Ipld>,
    pairs: impl IntoIterator<Item = (&'a [u8], &'a [u8])>,
) -> Result<Vec<u8>, StarError> {
    let mut out = encode_header(meta)?;
    for (key, value) in pairs {
        out.extend(encode_pair(key, value));
    }
    out.extend(END);
    Ok(out)
}

fn frame(out: &mut Vec<u8>, bytes: &[u8]) {
    encode_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn encode_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Reads an archive: the header when opened, then the pairs one at a time.
pub struct StarReader<R> {
    input: R,
    limits: Limits,
    meta: BTreeMap<String, Ipld>,
    done: bool,
}

impl<R: Read> StarReader<R> {
    /// Read the magic bytes and the metadata.
    pub fn open(mut input: R, limits: Limits) -> Result<Self, StarError> {
        let mut magic = [0u8; 4];
        read_exact(&mut input, &mut magic)?;
        if magic != MAGIC {
            return Err(StarError::InvalidMagic);
        }
        let meta_bytes = read_frame(&mut input, "metadata", limits.max_meta)?;
        let meta = parse_meta(&meta_bytes)?;
        Ok(Self {
            input,
            limits,
            meta,
            done: false,
        })
    }

    /// The metadata map.
    pub fn meta(&self) -> &BTreeMap<String, Ipld> {
        &self.meta
    }

    /// The metadata's `$type`, naming the archive's profile.
    pub fn profile(&self) -> &str {
        match self.meta.get("$type") {
            Some(Ipld::String(t)) => t,
            _ => unreachable!("parse_meta requires a string $type"),
        }
    }

    /// The next pair, or `None` once the end byte has been read and nothing
    /// follows it.
    pub fn next_pair(&mut self) -> Result<Option<Pair>, StarError> {
        if self.done {
            return Ok(None);
        }
        match read_byte(&mut self.input)? {
            CONTROL_END => {
                self.done = true;
                let mut trailing = [0u8; 1];
                if self.input.read(&mut trailing)? != 0 {
                    return Err(StarError::TrailingBytes);
                }
                Ok(None)
            }
            CONTROL_PAIR => {
                let key = read_frame(&mut self.input, "key", self.limits.max_key)?;
                let value = read_frame(&mut self.input, "value", self.limits.max_value)?;
                Ok(Some(Pair { key, value }))
            }
            other => Err(StarError::InvalidControlByte(other)),
        }
    }
}

impl<R: Read> Iterator for StarReader<R> {
    type Item = Result<Pair, StarError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_pair() {
            Ok(Some(pair)) => Some(Ok(pair)),
            Ok(None) => None,
            Err(err) => {
                self.done = true;
                Some(Err(err))
            }
        }
    }
}

fn parse_meta(bytes: &[u8]) -> Result<BTreeMap<String, Ipld>, StarError> {
    let meta: Ipld =
        serde_ipld_dagcbor::from_slice(bytes).map_err(|e| StarError::InvalidMeta(e.to_string()))?;
    let Ipld::Map(map) = meta else {
        return Err(StarError::InvalidMeta("not a map".into()));
    };
    if !matches!(map.get("$type"), Some(Ipld::String(_))) {
        return Err(StarError::InvalidMeta("missing a string $type".into()));
    }
    // The decoder accepts some non-canonical forms, such as unsorted or
    // duplicated map keys, so the metadata has to re-encode to its own bytes.
    let reencoded = serde_ipld_dagcbor::to_vec(&Ipld::Map(map.clone()))
        .map_err(|e| StarError::InvalidMeta(e.to_string()))?;
    if reencoded != bytes {
        return Err(StarError::NonCanonicalMeta);
    }
    Ok(map)
}

/// Read a length-prefixed frame. The length is checked before anything is
/// buffered, and the bytes are read as they arrive rather than allocated up
/// front, so a frame that claims a huge length costs only what the input holds.
fn read_frame(input: &mut impl Read, what: &'static str, limit: u64) -> Result<Vec<u8>, StarError> {
    let length = read_varint(input)?;
    if length > limit {
        return Err(StarError::TooLong {
            what,
            length,
            limit,
        });
    }
    let mut bytes = Vec::new();
    input.take(length).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != length {
        return Err(StarError::UnexpectedEnd);
    }
    Ok(bytes)
}

fn read_varint(input: &mut impl Read) -> Result<u64, StarError> {
    let mut value: u64 = 0;
    for i in 0..MAX_VARINT_BYTES {
        let byte = read_byte(input)?;
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte < 0x80 {
            // A final zero group only pads the encoding, which STAR forbids.
            if byte == 0 && i > 0 {
                return Err(StarError::NonCanonicalVarint);
            }
            return Ok(value);
        }
    }
    Err(StarError::VarintTooLong)
}

fn read_byte(input: &mut impl Read) -> Result<u8, StarError> {
    let mut byte = [0u8; 1];
    read_exact(input, &mut byte)?;
    Ok(byte[0])
}

fn read_exact(input: &mut impl Read, buf: &mut [u8]) -> Result<(), StarError> {
    input.read_exact(buf).map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => StarError::UnexpectedEnd,
        _ => StarError::Io(e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(profile: &str) -> BTreeMap<String, Ipld> {
        BTreeMap::from([("$type".to_string(), Ipld::String(profile.into()))])
    }

    #[test]
    fn varints_encode_in_their_shortest_form() {
        for (value, expected) in [
            (0u64, vec![0x00]),
            (127, vec![0x7f]),
            (128, vec![0x80, 0x01]),
            (300, vec![0xac, 0x02]),
        ] {
            let mut out = Vec::new();
            encode_varint(&mut out, value);
            assert_eq!(out, expected, "{value}");
            assert_eq!(read_varint(&mut out.as_slice()).unwrap(), value);
        }
    }

    #[test]
    fn a_written_archive_reads_back() {
        let bytes = write_star(
            &meta("atproto-space-repo"),
            [(&b"app.bsky.feed.post/3k"[..], &b"\xa0"[..])],
        )
        .unwrap();
        let mut reader = StarReader::open(bytes.as_slice(), Limits::default()).unwrap();
        assert_eq!(reader.profile(), "atproto-space-repo");
        let pair = reader.next_pair().unwrap().unwrap();
        assert_eq!(pair.key, b"app.bsky.feed.post/3k");
        assert!(reader.next_pair().unwrap().is_none());
    }

    #[test]
    fn a_key_over_its_limit_is_refused_before_it_is_read() {
        let bytes = write_star(&meta("p"), [(&[0u8; 10][..], &b""[..])]).unwrap();
        let limits = Limits {
            max_key: 9,
            ..Limits::default()
        };
        let mut reader = StarReader::open(bytes.as_slice(), limits).unwrap();
        assert!(matches!(
            reader.next_pair(),
            Err(StarError::TooLong {
                what: "key",
                length: 10,
                limit: 9
            })
        ));
    }

    #[test]
    fn a_header_without_a_type_is_refused() {
        assert!(matches!(
            encode_header(&BTreeMap::new()),
            Err(StarError::InvalidMeta(_))
        ));
    }
}
