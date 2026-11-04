//! Which addresses an outbound connection may reach.
//!
//! Most outbound destinations are not the operator's choice: a PDS endpoint
//! comes from a DID document, a handle from whoever typed it, a webhook from a
//! stored row, a plugin's URL from the plugin. Any of those can name a host
//! that resolves to the instance's own network. So every client built by
//! [`crate::http_retry`] checks addresses twice:
//!
//! - its resolver drops every address that is not publicly routable, so a
//!   name that resolves only to such addresses is never connected to;
//! - after the connection is made, and before any request is written to it,
//!   it reads the socket's peer address and refuses a non-public one. This is
//!   the check
//!   that holds: the address judged is the address connected to, so DNS
//!   rebinding between the two cannot move it, and it also covers IP-literal
//!   URLs and redirect hops, which never reach a resolver.
//!
//! Hosts the operator configured (the PLC directory, relay, Jetstream,
//! telemetry, plugin registry and sources, and any proxy) are exempt by name,
//! and the addresses they resolve to are exempt at connect time, each only on
//! the port its URL names: a configured `http://127.0.0.1:2582` exempts that
//! one service, not everything else listening on the machine. This
//! instance's own `PUBLIC_URL` is not exempt; nothing it sends is addressed
//! to itself. The hosts in `ALLOW_PRIVATE_HOSTS` are exempt too, but only for
//! the clients plugins and scripts send their own requests through
//! ([`Source::Plugins`]): every other request keeps refusing them, by name and
//! by address, because its destination came from somewhere else. Behind a
//! proxy the peer is the proxy, so what the proxy connects to onwards is the
//! proxy's to decide.
//!
//! Nothing is filtered until [`install`] has run, which `run` does at boot, so
//! tests and one-shot tools that point at a local mock keep working. A redirect
//! to an IP literal is judged before it is followed, so it is never connected
//! to at all.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock};
use std::task::{Context, Poll};

use hyper_util::client::legacy::connect::{Connection, HttpInfo};

type BoxError = Box<dyn Error + Send + Sync>;

/// Turns the filter off, for an instance whose PDSes or labelers really are on
/// a private network. Read from the process environment only.
pub const ALLOW_PRIVATE_VAR: &str = "ALLOW_PRIVATE_DESTINATIONS";

/// Host names, or URLs whose host is taken, that plugins and scripts may reach
/// at a private address: services beside this instance that its scripts call.
/// Read from the process environment only.
pub const ALLOW_PRIVATE_HOSTS_VAR: &str = "ALLOW_PRIVATE_HOSTS";

/// The ports an exempt host may be reached on. A configured URL names one,
/// explicitly or by its scheme's default; only a bare host name allows any.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ports {
    Any,
    Only(HashSet<u16>),
}

impl Ports {
    fn allows(&self, port: u16) -> bool {
        match self {
            Ports::Any => true,
            Ports::Only(ports) => ports.contains(&port),
        }
    }

    fn merge(&mut self, other: &Ports) {
        match (&mut *self, other) {
            (Ports::Any, _) => {}
            (_, Ports::Any) => *self = Ports::Any,
            (Ports::Only(mine), Ports::Only(theirs)) => mine.extend(theirs),
        }
    }
}

#[derive(Debug, Default)]
pub struct Policy {
    trusted_names: HashMap<String, Ports>,
    trusted_addrs: RwLock<HashMap<IpAddr, Ports>>,
}

impl Policy {
    /// A policy exempting the hosts of `urls`, each on its URL's port (the
    /// scheme's default when none is written), or on any port for a bare host
    /// name. An IP-literal host is exempt as that address; a name is exempt
    /// along with whatever it resolves to, on the same ports.
    pub fn trusting<'a>(urls: impl IntoIterator<Item = &'a str>) -> Self {
        let mut names: HashMap<String, Ports> = HashMap::new();
        let mut addrs: HashMap<IpAddr, Ports> = HashMap::new();
        for raw in urls {
            let Some((host, ports)) = url_host(raw) else {
                continue;
            };
            match host {
                url::Host::Ipv4(v4) => merge_into(&mut addrs, canonical(IpAddr::V4(v4)), &ports),
                url::Host::Ipv6(v6) => merge_into(&mut addrs, canonical(IpAddr::V6(v6)), &ports),
                url::Host::Domain(name) => merge_into(
                    &mut names,
                    name.trim_end_matches('.').to_ascii_lowercase(),
                    &ports,
                ),
            }
        }
        Self {
            trusted_names: names,
            trusted_addrs: RwLock::new(addrs),
        }
    }

    fn name_ports(&self, host: &str) -> Option<&Ports> {
        self.trusted_names
            .get(&host.trim_end_matches('.').to_ascii_lowercase())
    }

    /// Whether `ip` may be connected to on `port`.
    fn admits(&self, ip: IpAddr, port: u16) -> bool {
        is_public(ip)
            || self
                .trusted_addrs
                .read()
                .map(|addrs| addrs.get(&canonical(ip)).is_some_and(|p| p.allows(port)))
                .unwrap_or(false)
    }

    /// Whether `ip` is exempt on any port: what a resolver, which does not know
    /// the port yet, can judge. The port is judged once connected.
    fn admits_on_some_port(&self, ip: IpAddr) -> bool {
        is_public(ip)
            || self
                .trusted_addrs
                .read()
                .map(|addrs| addrs.contains_key(&canonical(ip)))
                .unwrap_or(false)
    }
}

fn merge_into<K: std::hash::Hash + Eq>(map: &mut HashMap<K, Ports>, key: K, ports: &Ports) {
    map.entry(key)
        .and_modify(|p| p.merge(ports))
        .or_insert_with(|| ports.clone());
}

/// The host of a configured URL and the ports it names. A full URL names its
/// port, or its scheme's default; a bare `host:port`, as a proxy variable may
/// be, names that port; a bare host names none, and so allows any.
fn url_host(raw: &str) -> Option<(url::Host<String>, Ports)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let only = |port: u16| Ports::Only(HashSet::from([port]));
    if let Ok(url) = url::Url::parse(raw)
        && let Some(host) = url.host()
    {
        let ports = url.port_or_known_default().map_or(Ports::Any, only);
        return Some((host.to_owned(), ports));
    }
    // A bare `host:port` parses as a scheme and a path with no host at all.
    let url = url::Url::parse(&format!("http://{raw}")).ok()?;
    let host = url.host()?.to_owned();
    Some((host, url.port().map_or(Ports::Any, only)))
}

static POLICY: OnceLock<Arc<Policy>> = OnceLock::new();
static PLUGIN_POLICY: OnceLock<Arc<Policy>> = OnceLock::new();

/// Start filtering with `policy`, for plugins' own requests as well. Only the
/// first call has any effect.
pub fn install(policy: Policy) {
    install_with_plugins(policy, None);
}

/// Start filtering with `policy`, and with `plugins` for the requests plugins
/// and scripts send themselves (`policy` when `None`). Each keeps its own
/// record of trusted addresses, so an address only the plugin policy trusts
/// never becomes trusted for anything else. Only the first call has any
/// effect.
pub fn install_with_plugins(policy: Policy, plugins: Option<Policy>) {
    let policy = Arc::new(policy);
    if POLICY.set(policy.clone()).is_ok() {
        let _ = PLUGIN_POLICY.set(plugins.map(Arc::new).unwrap_or(policy));
    }
}

/// Where a client finds its policy: the one [`install`] set, read at each
/// lookup and connect so a client built before boot installed it is still
/// covered; the one installed for plugins' own requests; or a fixed one, which
/// is how tests exercise the filter without installing it for their whole
/// binary.
#[derive(Clone, Debug, Default)]
pub enum Source {
    #[default]
    Installed,
    Plugins,
    Fixed(Arc<Policy>),
}

impl Source {
    fn policy(&self) -> Option<&Policy> {
        match self {
            Source::Installed => POLICY.get().map(Arc::as_ref),
            Source::Plugins => PLUGIN_POLICY.get().map(Arc::as_ref),
            Source::Fixed(policy) => Some(policy),
        }
    }
}

/// Add the resolver, the connect-time check and the redirect check to
/// `builder`. A client that follows redirects is given `max_redirects`;
/// `None` keeps whatever redirect policy the builder already has.
pub fn screened(
    builder: reqwest::ClientBuilder,
    source: Source,
    max_redirects: Option<usize>,
) -> reqwest::ClientBuilder {
    let builder = builder
        .dns_resolver(Arc::new(ScreeningResolver(source.clone())))
        .connector_layer(CheckPeerLayer(source.clone()));
    match max_redirects {
        Some(max) => builder.redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= max {
                attempt.error(format!("too many redirects (more than {max})"))
            } else if !redirect_allowed(&source, attempt.url()) {
                let refused = format!(
                    "refusing redirect to {}: not a public address",
                    attempt.url().host_str().unwrap_or_default()
                );
                attempt.error(refused)
            } else {
                attempt.follow()
            }
        })),
        None => builder,
    }
}

/// Whether the filter should be off, from the process environment alone:
/// these are never read through `config::vars`, so neither a project file nor
/// anything a dashboard user or plugin can set reaches them.
/// `HAPPYVIEW_NETWORK=local` is what `hpy happyview dev --network local` sets,
/// where the PDS and PLC are on this machine, and counts only under `dev`.
pub fn private_destinations_allowed(dev: bool) -> bool {
    let allow = std::env::var(ALLOW_PRIVATE_VAR)
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    let local_network = std::env::var("HAPPYVIEW_NETWORK").is_ok_and(|v| v.trim() == "local");
    allow || (dev && local_network)
}

/// Install the policy for this instance's configuration, or say loudly why
/// not.
pub fn install_for(config: &crate::config::Config, dev: bool) {
    if private_destinations_allowed(dev) {
        tracing::warn!(
            "outbound requests are not screened for private, loopback and link-local \
             addresses ({ALLOW_PRIVATE_VAR}, or HAPPYVIEW_NETWORK=local under dev, is set), \
             including requests to endpoints from DID documents, handles, plugins and \
             stored URLs"
        );
        return;
    }
    let mut urls: Vec<String> = vec![
        config.plc_url.clone(),
        config.relay_url.clone(),
        config.jetstream_url.clone(),
        config.telemetry_collector_url.clone(),
    ];
    urls.extend(config.default_plugins.iter().cloned());
    for var in ["PLUGIN_REGISTRY_URL", "PLUGIN_URLS"] {
        if let Some(value) = crate::config::vars::var(var) {
            urls.extend(value.split(',').map(str::to_string));
        }
    }
    for var in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(value) = std::env::var(var) {
            urls.push(value);
        }
    }
    let private_hosts = private_hosts(std::env::var(ALLOW_PRIVATE_HOSTS_VAR).ok().as_deref());
    let plugins = if private_hosts.is_empty() {
        None
    } else {
        tracing::info!(
            hosts = %private_hosts.join(","),
            "{ALLOW_PRIVATE_HOSTS_VAR}: plugins and scripts may reach these hosts at private addresses"
        );
        Some(Policy::trusting(
            urls.iter().chain(&private_hosts).map(String::as_str),
        ))
    };
    install_with_plugins(Policy::trusting(urls.iter().map(String::as_str)), plugins);
}

/// The entries of `ALLOW_PRIVATE_HOSTS`: comma-separated, blanks dropped.
pub fn private_hosts(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether `ip` is publicly routable. Everything else (loopback, private,
/// shared, link-local, documentation, benchmarking, multicast and reserved
/// space) is refused, and an IPv6 address that embeds an IPv4 one is judged
/// as that IPv4 address.
pub fn is_public(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

/// An IPv4-mapped IPv6 address reaches the IPv4 host, so it is judged, and
/// remembered, as that host.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => ip,
        },
        v4 => v4,
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0 // "this network", which Linux routes to the local host
        || a == 10
        || a == 127
        || (a == 100 && (64..128).contains(&b)) // shared address space
        || (a == 169 && b == 254)
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19)) // benchmarking
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224) // multicast, reserved and broadcast
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let embedded_v4 =
        |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        // NAT64 well-known prefix: the gateway connects to the embedded host.
        return is_public_v4(embedded_v4(s[6], s[7]));
    }
    if s[0] == 0x2002 {
        // 6to4 carries its IPv4 relay address in the next 32 bits.
        return is_public_v4(embedded_v4(s[1], s[2]));
    }
    // Global unicast is 2000::/3; everything outside it is local, multicast
    // or unassigned, which covers ::, ::1, IPv4-compatible addresses, fc00::/7,
    // fe80::/10 and ff00::/8.
    if s[0] & 0xe000 != 0x2000 {
        return false;
    }
    !(
        // Teredo, whose client address is obscured; benchmarking; ORCHID.
        (s[0] == 0x2001 && s[1] == 0)
            || (s[0] == 0x2001 && s[1] == 0x0002 && s[2] == 0)
            || (s[0] == 0x2001 && (s[1] & 0xfff0) == 0x0010)
            || (s[0] == 0x2001 && (s[1] & 0xfff0) == 0x0020)
            // Documentation: 2001:db8::/32 and 3fff::/20.
            || (s[0] == 0x2001 && s[1] == 0x0db8)
            || (s[0] & 0xfff0) == 0x3ff0
    )
}

/// Keep only the addresses `host` may be connected at. A host the operator
/// configured keeps all of them, and they become exempt at connect time.
fn screen(source: &Source, host: &str, addrs: Vec<SocketAddr>) -> io::Result<Vec<SocketAddr>> {
    let Some(policy) = source.policy() else {
        return Ok(addrs);
    };
    if let Some(ports) = policy.name_ports(host) {
        if let Ok(mut trusted) = policy.trusted_addrs.write() {
            for addr in &addrs {
                merge_into(&mut trusted, canonical(addr.ip()), ports);
            }
        }
        return Ok(addrs);
    }
    let public: Vec<SocketAddr> = addrs
        .into_iter()
        .filter(|a| policy.admits_on_some_port(a.ip()))
        .collect();
    if public.is_empty() {
        return Err(refusal(format!("{host} resolves to no public address")));
    }
    Ok(public)
}

fn refusal(message: String) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("refusing outbound connection: {message}"),
    )
}

/// Refuse a connected peer the policy does not admit. `None` means the
/// connection could not say where it went, which is refused too.
fn check_peer(source: &Source, peer: Option<SocketAddr>) -> io::Result<()> {
    let Some(policy) = source.policy() else {
        return Ok(());
    };
    match peer {
        Some(addr) if policy.admits(addr.ip(), addr.port()) => Ok(()),
        Some(addr) => Err(refusal(format!("{} is not a public address", addr.ip()))),
        None => Err(refusal("the peer address could not be read".to_string())),
    }
}

/// Whether a redirect to `url` may be followed. Only an IP literal can be
/// judged here; a name is judged when it resolves. Refusing the literal now
/// saves connecting to it at all.
fn redirect_allowed(source: &Source, url: &url::Url) -> bool {
    let Some(policy) = source.policy() else {
        return true;
    };
    let port = url.port_or_known_default().unwrap_or(0);
    match url.host() {
        Some(url::Host::Ipv4(v4)) => policy.admits(IpAddr::V4(v4), port),
        Some(url::Host::Ipv6(v6)) => policy.admits(IpAddr::V6(v6), port),
        _ => true,
    }
}

/// Resolve `host` and connect to the first address the policy admits, for
/// clients that open their own sockets rather than going through reqwest.
pub async fn connect_tcp(
    source: &Source,
    host: &str,
    port: u16,
) -> io::Result<tokio::net::TcpStream> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> = match bare.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((bare, port)).await?.collect(),
    };
    let addrs = screen(source, bare, addrs)?;
    let mut last = None;
    for addr in addrs {
        match tokio::net::TcpStream::connect(addr).await {
            Ok(stream) => {
                check_peer(source, stream.peer_addr().ok())?;
                return Ok(stream);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other(format!("{host} has no addresses"))))
}

#[derive(Clone, Debug)]
struct ScreeningResolver(Source);

impl reqwest::dns::Resolve for ScreeningResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let source = self.0.clone();
        Box::pin(async move {
            // Port 0 is a placeholder; reqwest substitutes the URL's port.
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let addrs = screen(&source, &host, addrs)?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Checks the peer of each new connection before reqwest writes to it.
#[derive(Clone, Debug)]
struct CheckPeerLayer(Source);

impl<S> tower::Layer<S> for CheckPeerLayer {
    type Service = CheckPeer<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CheckPeer(inner, self.0.clone())
    }
}

#[derive(Clone, Debug)]
struct CheckPeer<S>(S, Source);

impl<S, R> tower::Service<R> for CheckPeer<S>
where
    S: tower::Service<R, Error = BoxError>,
    S::Response: Connection + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, BoxError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, req: R) -> Self::Future {
        let connecting = self.0.call(req);
        let source = self.1.clone();
        Box::pin(async move {
            let conn = connecting.await?;
            let mut extras = axum::http::Extensions::new();
            conn.connected().get_extras(&mut extras);
            check_peer(&source, extras.get::<HttpInfo>().map(HttpInfo::remote_addr))?;
            Ok(conn)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_publicly_routable_addresses_are_public() {
        for public in [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.0",
            "172.15.255.255",
            "172.32.0.0",
            "192.0.1.1",
            "198.17.255.255",
            "198.20.0.0",
            "223.255.255.254",
            "2606:4700:4700::1111",
            "2a00:1450:4001:80b::200e",
            "64:ff9b::808:808",
            "2002:808:808::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public(ip(public)), "{public} should be public");
        }
        for private in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "127.0.0.1",
            "100.64.0.1",
            "100.127.255.255",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::10.0.0.1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "3fff:e2e::1",
            "2001::1",
            "2001:2::1",
            "2001:10::1",
            "2001:20::1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::1",
            "2002:a00:1::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.1.2.3",
        ] {
            assert!(!is_public(ip(private)), "{private} should not be public");
        }
    }

    #[test]
    fn a_connection_whose_peer_cannot_be_read_is_refused() {
        let source = Source::Fixed(Arc::new(Policy::default()));
        let err = check_peer(&source, None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(check_peer(&source, Some("8.8.8.8:443".parse().unwrap())).is_ok());
    }

    #[test]
    fn a_trusted_host_is_matched_by_name_or_by_its_literal_address_on_its_port() {
        let policy = Policy::trusting([
            "http://plc:2582",
            "https://Relay.Example.",
            "http://10.9.8.7:6000/x",
            "http://[fd00::5]",
            "proxy.internal:3128",
            "sidecar",
            "",
            "not a url at all ::",
        ]);
        let only = |p: u16| Some(Ports::Only(HashSet::from([p])));
        assert_eq!(policy.name_ports("plc").cloned(), only(2582));
        assert_eq!(policy.name_ports("relay.example").cloned(), only(443));
        assert_eq!(policy.name_ports("RELAY.EXAMPLE.").cloned(), only(443));
        assert_eq!(policy.name_ports("proxy.internal").cloned(), only(3128));
        assert_eq!(policy.name_ports("sidecar").cloned(), Some(Ports::Any));
        assert!(policy.name_ports("evil.example").is_none());

        assert!(policy.admits(ip("10.9.8.7"), 6000));
        assert!(policy.admits(ip("::ffff:10.9.8.7"), 6000));
        assert!(!policy.admits(ip("10.9.8.7"), 5432));
        assert!(policy.admits(ip("fd00::5"), 80));
        assert!(!policy.admits(ip("fd00::5"), 22));
        assert!(!policy.admits(ip("10.9.8.8"), 6000));
        assert!(policy.admits(ip("8.8.8.8"), 5432));
        assert!(policy.admits_on_some_port(ip("10.9.8.7")));
        assert!(!policy.admits_on_some_port(ip("10.9.8.8")));
    }

    #[test]
    fn a_name_lends_its_ports_to_the_addresses_it_resolves_to() {
        let source = Source::Fixed(Arc::new(Policy::trusting(["http://plc:2582", "sidecar"])));
        let addrs = vec!["10.0.0.5:0".parse().unwrap()];
        screen(&source, "plc", addrs.clone()).unwrap();
        assert!(check_peer(&source, Some("10.0.0.5:2582".parse().unwrap())).is_ok());
        assert!(check_peer(&source, Some("10.0.0.5:5432".parse().unwrap())).is_err());
        screen(&source, "sidecar", vec!["10.0.0.6:0".parse().unwrap()]).unwrap();
        assert!(check_peer(&source, Some("10.0.0.6:9999".parse().unwrap())).is_ok());
        // An untrusted name resolving to a trusted address keeps it, so that
        // address may be connected to on its trusted port and no other.
        assert_eq!(screen(&source, "elsewhere", addrs).unwrap().len(), 1);
    }
}
