//! Jetstream settings from the environment, and the subscription URLs built
//! from them.

use std::time::Duration;

use crate::record_handler::LEXICON_SCHEMA_COLLECTION;

/// Most collections either protocol takes in one subscription. Past it, v2
/// refuses the subscription with `InvalidRequest`, so the filter is dropped
/// and unregistered collections are discarded on arrival instead.
pub const MAX_COLLECTION_FILTERS: usize = 100;
pub const DEFAULT_WORKERS: usize = 8;
pub const MAX_WORKERS: usize = 64;
/// How long to wait after the server rejects the subscription as invalid,
/// which retrying sooner will not fix.
pub const INVALID_REQUEST_RETRY: Duration = Duration::from_secs(3600);

pub const V2_SUBSCRIBE_PATH: &str = "/xrpc/network.bsky.jetstream.subscribeEvents";
pub const V1_SUBSCRIBE_PATH: &str = "/subscribe";
pub const DICTIONARY_PATH: &str = "/xrpc/network.bsky.jetstream.getZstdDictionary";

/// `JETSTREAM_PROTOCOL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolChoice {
    /// v2, falling back to v1 when the host has no v2 endpoint.
    Auto,
    V2,
    V1,
}

/// The protocol a connection actually speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    V2,
    V1,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::V2 => "v2",
            Protocol::V1 => "v1",
        }
    }
}

pub fn parse_protocol(raw: Option<&str>) -> ProtocolChoice {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("auto") => ProtocolChoice::Auto,
        Some("v2") => ProtocolChoice::V2,
        Some("v1") => ProtocolChoice::V1,
        Some(other) => {
            tracing::warn!(
                value = other,
                "JETSTREAM_PROTOCOL must be auto, v2 or v1; using auto"
            );
            ProtocolChoice::Auto
        }
    }
}

pub fn parse_compression(raw: Option<&str>) -> bool {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("true" | "1" | "yes" | "on") => true,
        Some("false" | "0" | "no" | "off") => false,
        Some(other) => {
            tracing::warn!(
                value = other,
                "JETSTREAM_COMPRESSION must be true or false; using true"
            );
            true
        }
    }
}

pub fn parse_workers(raw: Option<&str>) -> usize {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return DEFAULT_WORKERS;
    };
    match raw.parse::<usize>() {
        Ok(n) if n >= 1 => n.min(MAX_WORKERS),
        _ => {
            tracing::warn!(
                value = raw,
                default = DEFAULT_WORKERS,
                "JETSTREAM_WORKERS must be a positive integer; using the default"
            );
            DEFAULT_WORKERS
        }
    }
}

/// A Jetstream host, reachable as a websocket and over HTTP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// `wss://host` (or `ws://` for a plain-HTTP host).
    pub ws_base: String,
    /// `https://host` (or `http://`), for `getZstdDictionary`.
    pub http_base: String,
    /// `host[:port]`: which Jetstream instance a saved seq belongs to.
    pub host: String,
}

/// Read `JETSTREAM_URL` as a host. Any scheme of ws, wss, http or https (wss
/// when none), and a trailing v1 or v2 subscribe path, query or slash, all
/// name the same host.
pub fn parse_endpoint(url: &str) -> Endpoint {
    let url = url.trim();
    let url = url.split(['?', '#']).next().unwrap_or_default();
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => ("wss".to_string(), url),
    };
    let mut rest = rest.trim_end_matches('/');
    for suffix in [V2_SUBSCRIBE_PATH, V1_SUBSCRIBE_PATH] {
        if let Some(stripped) = rest.strip_suffix(suffix) {
            rest = stripped;
            break;
        }
    }
    let rest = rest.trim_end_matches('/');
    let (ws, http) = match scheme.as_str() {
        "ws" | "http" => ("ws", "http"),
        _ => ("wss", "https"),
    };
    let host = rest.to_ascii_lowercase();
    Endpoint {
        ws_base: format!("{ws}://{host}"),
        http_base: format!("{http}://{host}"),
        host,
    }
}

/// The registered collections plus the lexicon-schema collection, sorted and
/// deduplicated, or `None` when there are more than one filter takes.
fn collection_filters(collections: &[String]) -> Option<Vec<&str>> {
    let mut wanted: Vec<&str> = collections.iter().map(String::as_str).collect();
    wanted.push(LEXICON_SCHEMA_COLLECTION);
    wanted.sort_unstable();
    wanted.dedup();
    if wanted.len() > MAX_COLLECTION_FILTERS {
        tracing::warn!(
            count = wanted.len(),
            limit = MAX_COLLECTION_FILTERS,
            "more collections than a jetstream filter takes; subscribing to every commit"
        );
        None
    } else {
        Some(wanted)
    }
}

/// The v2 `subscribeEvents` URL. No `kinds` filter: `collections` only
/// constrains commits, so account, identity and sync events still arrive.
pub fn v2_url(
    endpoint: &Endpoint,
    collections: &[String],
    cursor: Option<i64>,
    dictionary_id: Option<u32>,
) -> String {
    let mut params: Vec<String> = Vec::new();
    if let Some(wanted) = collection_filters(collections) {
        params.extend(wanted.iter().map(|c| format!("collections={c}")));
    }
    if let Some(cursor) = cursor {
        params.push(format!("cursor={cursor}"));
    }
    if let Some(id) = dictionary_id {
        params.push(format!("zstdDictionary={id}"));
    }
    let base = format!("{}{V2_SUBSCRIBE_PATH}", endpoint.ws_base);
    if params.is_empty() {
        base
    } else {
        format!("{base}?{}", params.join("&"))
    }
}

/// The v1 `/subscribe` URL, uncompressed as HappyView has always used it.
pub fn v1_url(endpoint: &Endpoint, collections: &[String], cursor: Option<i64>) -> String {
    let mut params = vec!["compress=false".to_string()];
    if let Some(wanted) = collection_filters(collections) {
        params.extend(wanted.iter().map(|c| format!("wantedCollections={c}")));
    }
    if let Some(cursor) = cursor {
        params.push(format!("cursor={cursor}"));
    }
    format!(
        "{}{V1_SUBSCRIBE_PATH}?{}",
        endpoint.ws_base,
        params.join("&")
    )
}

pub fn dictionary_url(endpoint: &Endpoint) -> String {
    format!("{}{DICTIONARY_PATH}", endpoint.http_base)
}

/// Everything the consumer reads from the environment.
#[derive(Clone, Debug)]
pub struct Settings {
    pub endpoint: Endpoint,
    pub protocol: ProtocolChoice,
    pub compression: bool,
    pub workers: usize,
    pub invalid_request_retry: Duration,
}

impl Settings {
    pub fn from_env(jetstream_url: &str) -> Self {
        let var = |name: &str| std::env::var(name).ok();
        Self {
            endpoint: parse_endpoint(jetstream_url),
            protocol: parse_protocol(var("JETSTREAM_PROTOCOL").as_deref()),
            compression: parse_compression(var("JETSTREAM_COMPRESSION").as_deref()),
            workers: parse_workers(var("JETSTREAM_WORKERS").as_deref()),
            invalid_request_retry: INVALID_REQUEST_RETRY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_defaults_to_auto() {
        assert_eq!(parse_protocol(None), ProtocolChoice::Auto);
        assert_eq!(parse_protocol(Some("auto")), ProtocolChoice::Auto);
        assert_eq!(parse_protocol(Some(" V2 ")), ProtocolChoice::V2);
        assert_eq!(parse_protocol(Some("v1")), ProtocolChoice::V1);
        assert_eq!(parse_protocol(Some("v3")), ProtocolChoice::Auto);
    }

    #[test]
    fn compression_is_on_unless_turned_off() {
        assert!(parse_compression(None));
        assert!(parse_compression(Some("true")));
        assert!(parse_compression(Some("banana")));
        for off in ["false", "0", "no", "OFF"] {
            assert!(!parse_compression(Some(off)), "{off}");
        }
    }

    #[test]
    fn workers_default_to_eight_and_clamp() {
        assert_eq!(parse_workers(None), 8);
        assert_eq!(parse_workers(Some("0")), 8);
        assert_eq!(parse_workers(Some("x")), 8);
        assert_eq!(parse_workers(Some("3")), 3);
        assert_eq!(parse_workers(Some("1000")), 64);
    }

    fn endpoint(ws: &str, http: &str, host: &str) -> Endpoint {
        Endpoint {
            ws_base: ws.into(),
            http_base: http.into(),
            host: host.into(),
        }
    }

    #[test]
    fn endpoints_accept_a_host_or_a_legacy_subscribe_url() {
        let hosted = "jetstream1.us-east.bsky.network";
        assert_eq!(
            parse_endpoint(&format!("wss://{hosted}")),
            endpoint(
                &format!("wss://{hosted}"),
                &format!("https://{hosted}"),
                hosted
            )
        );
        assert_eq!(
            parse_endpoint(&format!("wss://{hosted}/subscribe")),
            endpoint(
                &format!("wss://{hosted}"),
                &format!("https://{hosted}"),
                hosted
            ),
            "a legacy v1 URL means the same host"
        );
        assert_eq!(
            parse_endpoint("https://js.example.com/"),
            endpoint(
                "wss://js.example.com",
                "https://js.example.com",
                "js.example.com"
            )
        );
        assert_eq!(
            parse_endpoint("ws://127.0.0.1:6008/xrpc/network.bsky.jetstream.subscribeEvents"),
            endpoint(
                "ws://127.0.0.1:6008",
                "http://127.0.0.1:6008",
                "127.0.0.1:6008"
            )
        );
        assert_eq!(
            parse_endpoint("jetstream.example.com"),
            endpoint(
                "wss://jetstream.example.com",
                "https://jetstream.example.com",
                "jetstream.example.com"
            )
        );
        assert_eq!(
            parse_endpoint("wss://js.example.com/subscribe?wantedCollections=app.x").host,
            "js.example.com"
        );
    }

    #[test]
    fn endpoint_hosts_are_case_insensitive() {
        assert_eq!(
            parse_endpoint("WSS://JS.Example.com"),
            parse_endpoint("js.example.com")
        );
    }

    #[test]
    fn a_v2_url_filters_collections_and_carries_cursor_and_dictionary() {
        let ep = parse_endpoint("wss://js.example.com");
        assert_eq!(
            v2_url(&ep, &["app.test.post".to_string()], Some(42), Some(7)),
            "wss://js.example.com/xrpc/network.bsky.jetstream.subscribeEvents\
             ?collections=app.test.post&collections=com.atproto.lexicon.schema&cursor=42&zstdDictionary=7"
        );
        assert_eq!(
            v2_url(&ep, &[], None, None),
            "wss://js.example.com/xrpc/network.bsky.jetstream.subscribeEvents\
             ?collections=com.atproto.lexicon.schema"
        );
        let with_schema = v2_url(
            &ep,
            &[
                "com.atproto.lexicon.schema".to_string(),
                "app.test.post".to_string(),
            ],
            None,
            None,
        );
        assert_eq!(with_schema.matches("com.atproto.lexicon.schema").count(), 1);
        assert!(
            !with_schema.contains("kinds="),
            "no kinds filter: account, identity and sync events must arrive"
        );
    }

    #[test]
    fn too_many_collections_drop_the_filter() {
        let ep = parse_endpoint("wss://js.example.com");
        let many: Vec<String> = (0..100).map(|i| format!("app.test.c{i}")).collect();
        let v2 = v2_url(&ep, &many, Some(5), None);
        assert!(!v2.contains("collections="), "{v2}");
        assert!(v2.ends_with("subscribeEvents?cursor=5"), "{v2}");
        let v1 = v1_url(&ep, &many, None);
        assert!(!v1.contains("wantedCollections="), "{v1}");
    }

    #[test]
    fn exactly_the_limit_keeps_the_filter() {
        let ep = parse_endpoint("wss://js.example.com");
        let ninety_nine: Vec<String> = (0..99).map(|i| format!("app.test.c{i}")).collect();
        assert_eq!(
            v2_url(&ep, &ninety_nine, None, None)
                .matches("collections=")
                .count(),
            100
        );
    }

    #[test]
    fn a_v1_url_keeps_the_legacy_shape() {
        let ep = parse_endpoint("wss://js.example.com/subscribe");
        assert_eq!(
            v1_url(
                &ep,
                &["app.test.post".to_string()],
                Some(1_700_000_000_000_000)
            ),
            "wss://js.example.com/subscribe?compress=false&wantedCollections=app.test.post\
             &wantedCollections=com.atproto.lexicon.schema&cursor=1700000000000000"
        );
    }

    #[test]
    fn the_dictionary_is_fetched_over_http() {
        assert_eq!(
            dictionary_url(&parse_endpoint("wss://js.example.com")),
            "https://js.example.com/xrpc/network.bsky.jetstream.getZstdDictionary"
        );
    }
}
