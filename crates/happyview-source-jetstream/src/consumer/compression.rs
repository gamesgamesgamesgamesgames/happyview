//! Dictionary zstd for v2 frames: each binary frame is one zstd frame whose
//! content is exactly the JSON text frame.

use std::io::Read;
use std::time::Duration;

/// Largest decoded frame accepted (the Go client's read limit), so a hostile
/// or broken frame cannot decompress into unbounded memory.
pub(crate) const MAX_DECODED_FRAME: usize = 32 << 20;
/// A real dictionary is tens of KiB; refuse anything absurd.
const MAX_DICTIONARY_BYTES: usize = 1 << 20;
/// log2 of the largest window a frame may declare (32 MiB), so a hostile frame
/// cannot make the decoder allocate a huge window on top of the output cap.
const MAX_WINDOW_LOG: u32 = 25;
/// RFC 8878 §5 structured dictionary magic.
const DICTIONARY_MAGIC: [u8; 4] = [0x37, 0xA4, 0x30, 0xEC];

/// A structured zstd dictionary's ID: little-endian u32 after the magic.
/// `None` for anything else, and for ID 0, which is invalid.
pub(crate) fn dictionary_id(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 8 || bytes[..4] != DICTIONARY_MAGIC {
        return None;
    }
    let id = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    (id != 0).then_some(id)
}

pub(crate) struct Dictionary {
    pub id: u32,
    decoder: zstd::dict::DecoderDictionary<'static>,
}

impl std::fmt::Debug for Dictionary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dictionary").field("id", &self.id).finish()
    }
}

impl Dictionary {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let id = dictionary_id(bytes).ok_or("not a zstd dictionary with an id")?;
        Ok(Self {
            id,
            decoder: zstd::dict::DecoderDictionary::copy(bytes),
        })
    }

    /// Decode one binary frame into its JSON text.
    pub fn decode(&self, frame: &[u8]) -> Result<String, String> {
        let mut decoder =
            zstd::stream::read::Decoder::with_prepared_dictionary(frame, &self.decoder)
                .map_err(|e| e.to_string())?
                .single_frame();
        decoder
            .window_log_max(MAX_WINDOW_LOG)
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        decoder
            .take(MAX_DECODED_FRAME as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| e.to_string())?;
        if out.len() > MAX_DECODED_FRAME {
            return Err(format!("frame decodes past {MAX_DECODED_FRAME} bytes"));
        }
        String::from_utf8(out).map_err(|e| e.to_string())
    }
}

/// `GET getZstdDictionary`: the server's current dictionary.
pub(crate) async fn fetch_dictionary(
    http: &reqwest::Client,
    url: &str,
) -> Result<Dictionary, String> {
    let response = http
        .get(url)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("dictionary fetch answered {}", response.status()));
    }
    let bytes = read_capped(response, MAX_DICTIONARY_BYTES).await?;
    Dictionary::from_bytes(&bytes)
}

pub use happyview_provider::http::read_capped;

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT: &str = "subscribe_events_20260811.zdict";
    const LEGACY: &str = "legacy_subscribe.zdict";

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/jetstream")
                .join(name),
        )
        .expect("dictionary fixture")
    }

    fn compress(dictionary: &[u8], data: &[u8]) -> Vec<u8> {
        zstd::bulk::Compressor::with_dictionary(3, dictionary)
            .expect("compressor")
            .compress(data)
            .expect("compress")
    }

    #[test]
    fn the_fixture_dictionaries_carry_their_ids() {
        assert_eq!(dictionary_id(&fixture(CURRENT)), Some(20_260_811));
        assert_eq!(dictionary_id(&fixture(LEGACY)), Some(1_612_007_021));
    }

    #[test]
    fn an_id_needs_the_magic_and_cannot_be_zero() {
        assert_eq!(dictionary_id(b"short"), None);
        assert_eq!(dictionary_id(&[0; 8]), None);
        assert_eq!(dictionary_id(&[0x37, 0xA4, 0x30, 0xEC, 0, 0, 0, 0]), None);
        assert_eq!(
            dictionary_id(&[0x37, 0xA4, 0x30, 0xEC, 7, 0, 0, 0]),
            Some(7)
        );
    }

    #[test]
    fn a_frame_round_trips_through_its_dictionary() {
        let bytes = fixture(CURRENT);
        let dictionary = Dictionary::from_bytes(&bytes).expect("dictionary");
        let text = r#"{"$type":"message","payload":{"$type":"network.bsky.jetstream.subscribeEvents#commit"}}"#;
        assert_eq!(
            dictionary
                .decode(&compress(&bytes, text.as_bytes()))
                .unwrap(),
            text
        );
    }

    #[test]
    fn a_frame_from_another_dictionary_is_refused() {
        let dictionary = Dictionary::from_bytes(&fixture(CURRENT)).expect("dictionary");
        let frame = compress(&fixture(LEGACY), b"{}");
        assert!(dictionary.decode(&frame).is_err());
    }

    #[test]
    fn a_frame_decoding_past_the_cap_is_refused() {
        let bytes = fixture(CURRENT);
        let dictionary = Dictionary::from_bytes(&bytes).expect("dictionary");
        let frame = compress(&bytes, &vec![b'a'; MAX_DECODED_FRAME + 1]);
        let err = dictionary.decode(&frame).unwrap_err();
        assert!(err.contains("past"), "{err}");
    }

    #[test]
    fn a_frame_declaring_a_larger_window_is_refused() {
        use std::io::Write;

        let bytes = fixture(CURRENT);
        let dictionary = Dictionary::from_bytes(&bytes).expect("dictionary");
        // With no pledged size the encoder declares the full 2^27 window.
        let mut encoder =
            zstd::stream::write::Encoder::with_dictionary(Vec::new(), 3, &bytes).expect("encoder");
        encoder.window_log(27).expect("window log");
        encoder.write_all(b"{}").expect("write");
        let frame = encoder.finish().expect("finish");
        assert!(dictionary.decode(&frame).is_err());
    }

    /// Ruling FR6: an oversized dictionary is refused while it is read, not
    /// after the whole body has arrived. Each server here then stalls; a
    /// fetch that waits for the end of the body hangs until its timeout.
    #[tokio::test]
    async fn an_oversized_dictionary_is_refused_while_it_is_read() {
        use tokio::io::AsyncWriteExt;

        async fn stalling_server(head: String, body: Vec<u8>) -> String {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("addr");
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut request = [0u8; 4096];
                let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut request).await;
                stream.write_all(head.as_bytes()).await.expect("head");
                stream.write_all(&body).await.expect("body");
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
            format!("http://{addr}/dictionary")
        }

        let declared = stalling_server(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                MAX_DICTIONARY_BYTES + 1
            ),
            Vec::new(),
        )
        .await;
        let mut chunk = format!("{:x}\r\n", MAX_DICTIONARY_BYTES + 1).into_bytes();
        chunk.extend(vec![0u8; MAX_DICTIONARY_BYTES + 1]);
        chunk.extend_from_slice(b"\r\n");
        let chunked = stalling_server(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_string(),
            chunk,
        )
        .await;

        let http = reqwest::Client::new();
        for url in [declared, chunked] {
            let err = tokio::time::timeout(Duration::from_secs(5), fetch_dictionary(&http, &url))
                .await
                .unwrap_or_else(|_| panic!("{url}: the fetch waited for the whole body"))
                .unwrap_err();
            assert!(err.contains("over"), "{url}: {err}");
        }
    }

    #[tokio::test]
    async fn the_dictionary_is_fetched_and_checked() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/network.bsky.jetstream.getZstdDictionary"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(fixture(CURRENT)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/not-a-dictionary"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();

        let fetched = fetch_dictionary(
            &http,
            &format!(
                "{}/xrpc/network.bsky.jetstream.getZstdDictionary",
                server.uri()
            ),
        )
        .await
        .expect("fetch");
        assert_eq!(fetched.id, 20_260_811);
        assert!(
            fetch_dictionary(&http, &format!("{}/missing", server.uri()))
                .await
                .is_err()
        );
        assert!(
            fetch_dictionary(&http, &format!("{}/not-a-dictionary", server.uri()))
                .await
                .is_err()
        );
    }
}
