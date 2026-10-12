//! Where the dashboard's static files come from: a directory on disk, or the
//! copy compiled into the binary.

use std::convert::Infallible;
use std::path::PathBuf;

use axum::Router;
use bytes::Bytes;
use http_body_util::Full;
use tower_http::services::ServeDir;

use crate::config::Config;

pub enum Source {
    Dir(PathBuf),
    Embedded,
}

/// `STATIC_DIR` always wins, so the Docker image and a customised dashboard
/// keep working. Unset, a binary built with the dashboard serves its own copy;
/// one built without keeps the source checkout's default.
pub fn source(static_dir: Option<&str>) -> Source {
    match static_dir {
        Some(dir) => Source::Dir(PathBuf::from(dir)),
        None if cfg!(feature = "embedded-dashboard") => Source::Embedded,
        None => Source::Dir(PathBuf::from("./web/out")),
    }
}

pub fn attach<S: Clone + Send + Sync + 'static>(router: Router<S>, config: &Config) -> Router<S> {
    match source(config.static_dir.as_deref()) {
        Source::Dir(dir) => dir_fallback(router, dir.to_string_lossy().into_owned()),
        Source::Embedded => embedded::attach(router, config.base_path.as_deref()),
    }
}

/// The files, relative to the export root, that stand in for a path with no
/// file of its own, best first: the parent's `_/index.html` (the shell Next.js
/// exports for a dynamic route), then the root `index.html`. Both sources call
/// this so they cannot disagree about the order.
fn spa_fallbacks(path: &str) -> Vec<String> {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let mut candidates = Vec::with_capacity(2);
    if segments.len() >= 2 {
        candidates.push(format!(
            "{}/_/index.html",
            segments[..segments.len() - 1].join("/")
        ));
    }
    candidates.push("index.html".to_string());
    candidates
}

/// `ServeDir` with an SPA fallback: a path with no file of its own gets the
/// parent's `_/index.html` (the shell Next.js exports for a dynamic route),
/// and failing that the root `index.html`.
fn dir_fallback<S: Clone + Send + Sync + 'static>(
    router: Router<S>,
    static_dir: String,
) -> Router<S> {
    let fallback_dir = static_dir.clone();
    let spa_fallback = tower::service_fn(move |req: axum::http::Request<_>| {
        let dir = fallback_dir.clone();
        async move {
            for rel in spa_fallbacks(req.uri().path()) {
                if let Ok(body) = tokio::fs::read(format!("{dir}/{rel}")).await {
                    return Ok::<_, Infallible>(
                        axum::http::Response::builder()
                            .header("content-type", "text/html; charset=utf-8")
                            .body(Full::new(Bytes::from(body)))
                            .unwrap(),
                    );
                }
            }
            Ok(axum::http::Response::builder()
                .header("content-type", "text/html; charset=utf-8")
                .body(Full::new(Bytes::new()))
                .unwrap())
        }
    });
    router.fallback_service(ServeDir::new(&static_dir).not_found_service(spa_fallback))
}

/// What the release build exports `NEXT_PUBLIC_BASE_PATH` as. The served copy
/// has it replaced by the configured base path, or removed.
pub const BASE_PATH_SENTINEL: &str = "/__HAPPYVIEW_BP__";

/// The extensions `entrypoint.sh` rewrites on disk; every other asset is
/// served byte for byte.
const REWRITTEN_EXTENSIONS: &[&str] = &["html", "js", "css", "txt"];

pub trait Assets: Send + Sync + 'static {
    fn get(&self, path: &str) -> Option<std::borrow::Cow<'static, [u8]>>;
}

pub struct EmbeddedDashboard<A: Assets> {
    assets: A,
    base_path: String,
    /// Rewritten text assets, so the sentinel is replaced once per file
    /// rather than on every request.
    rewritten: std::sync::Mutex<std::collections::HashMap<String, Bytes>>,
}

impl<A: Assets> EmbeddedDashboard<A> {
    pub fn new(assets: A, base_path: Option<&str>) -> Self {
        Self {
            assets,
            base_path: base_path.unwrap_or("").to_string(),
            rewritten: Default::default(),
        }
    }

    /// Mirrors `ServeDir` plus the directory source's SPA fallback: a
    /// directory path serves its `index.html`, a directory without its
    /// trailing slash redirects to it, and anything else unknown gets the
    /// dynamic-route shell or the root.
    pub fn respond(&self, path: &str) -> axum::response::Response {
        self.respond_to(path, None)
    }

    /// `respond`, keeping `query` on the redirect a bare directory gets.
    pub fn respond_to(&self, path: &str, query: Option<&str>) -> axum::response::Response {
        use axum::response::IntoResponse;

        let rel = path.trim_start_matches('/');
        if rel.is_empty() || rel.ends_with('/') {
            let index = format!("{rel}index.html");
            if let Some(res) = self.file(&index) {
                return res;
            }
        } else if let Some(res) = self.file(rel) {
            return res;
        } else if self.assets.get(&format!("{rel}/index.html")).is_some() {
            let location = match query {
                Some(q) => format!("{path}/?{q}"),
                None => format!("{path}/"),
            };
            return (
                axum::http::StatusCode::TEMPORARY_REDIRECT,
                [(axum::http::header::LOCATION, location)],
            )
                .into_response();
        }

        spa_fallbacks(path)
            .iter()
            .find_map(|candidate| self.file(candidate))
            .unwrap_or_else(|| axum::http::StatusCode::NOT_FOUND.into_response())
    }

    fn file(&self, rel: &str) -> Option<axum::response::Response> {
        let raw = self.assets.get(rel)?;
        let mime = mime_guess::from_path(rel).first_or_octet_stream();
        let rewrite = std::path::Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| REWRITTEN_EXTENSIONS.contains(&e));
        let bytes = match raw {
            std::borrow::Cow::Borrowed(raw) if !rewrite => Bytes::from_static(raw),
            std::borrow::Cow::Owned(raw) if !rewrite => Bytes::from(raw),
            // A text asset that is not UTF-8 cannot be searched for the
            // sentinel without corrupting it, so it goes out as it is.
            raw => match std::str::from_utf8(&raw) {
                Ok(text) => {
                    let mut cache = self.rewritten.lock().unwrap_or_else(|e| e.into_inner());
                    cache
                        .entry(rel.to_string())
                        .or_insert_with(|| {
                            Bytes::from(text.replace(BASE_PATH_SENTINEL, &self.base_path))
                        })
                        .clone()
                }
                Err(_) => Bytes::from(raw.into_owned()),
            },
        };
        Some(
            axum::http::Response::builder()
                .header(axum::http::header::CONTENT_TYPE, mime.as_ref())
                .body(axum::body::Body::from(bytes))
                .unwrap(),
        )
    }
}

#[cfg(feature = "embedded-dashboard")]
mod embedded {
    use std::sync::Arc;

    use axum::Router;

    use super::{Assets, EmbeddedDashboard};

    #[derive(rust_embed::Embed)]
    #[folder = "web/out/"]
    pub(super) struct Export;

    pub(super) struct RustEmbedAssets;

    impl Assets for RustEmbedAssets {
        fn get(&self, path: &str) -> Option<std::borrow::Cow<'static, [u8]>> {
            Export::get(path).map(|f| f.data)
        }
    }

    pub(super) fn attach<S: Clone + Send + Sync + 'static>(
        router: Router<S>,
        base_path: Option<&str>,
    ) -> Router<S> {
        let dashboard = Arc::new(EmbeddedDashboard::new(RustEmbedAssets, base_path));
        router.fallback(move |uri: axum::http::Uri| {
            let dashboard = dashboard.clone();
            async move { dashboard.respond_to(uri.path(), uri.query()) }
        })
    }
}

#[cfg(not(feature = "embedded-dashboard"))]
mod embedded {
    use axum::Router;

    pub(super) fn attach<S: Clone + Send + Sync + 'static>(
        _router: Router<S>,
        _base_path: Option<&str>,
    ) -> Router<S> {
        unreachable!("`source` answers Embedded only when the feature is on")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_dir_wins_whenever_it_is_set() {
        assert!(matches!(
            source(Some("/srv/static")),
            Source::Dir(p) if p == std::path::Path::new("/srv/static")
        ));
    }

    /// Without the embedded copy, an unset `STATIC_DIR` keeps today's
    /// default so a source checkout behaves as it always has.
    #[cfg(not(feature = "embedded-dashboard"))]
    #[test]
    fn an_unset_static_dir_falls_back_to_the_checkout() {
        assert!(matches!(
            source(None),
            Source::Dir(p) if p == std::path::Path::new("./web/out")
        ));
    }

    /// A directory's SPA fallback serves a dynamic route's `_/index.html`
    /// before the root shell, as Next's static export lays them out.
    #[tokio::test]
    async fn dir_source_serves_the_dynamic_route_shell() {
        use axum::body::Body;
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "root").unwrap();
        std::fs::create_dir_all(dir.path().join("dashboard/records/_")).unwrap();
        std::fs::write(dir.path().join("dashboard/records/_/index.html"), "shell").unwrap();

        let app = dir_fallback(
            axum::Router::<()>::new(),
            dir.path().to_str().unwrap().to_string(),
        );
        let res = app
            .clone()
            .oneshot(
                axum::http::Request::get("/dashboard/records/abc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.into_body().collect().await.unwrap().to_bytes(), "shell");

        let res = app
            .oneshot(
                axum::http::Request::get("/nowhere")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.into_body().collect().await.unwrap().to_bytes(), "root");
    }

    use std::borrow::Cow;
    use std::collections::HashMap;

    struct MapAssets(HashMap<&'static str, &'static [u8]>);

    impl Assets for MapAssets {
        fn get(&self, path: &str) -> Option<Cow<'static, [u8]>> {
            self.0.get(path).map(|b| Cow::Borrowed(*b))
        }
    }

    fn export() -> MapAssets {
        MapAssets(HashMap::from([
            (
                "index.html",
                b"<a href=\"/__HAPPYVIEW_BP__/dashboard/\">root</a>".as_slice(),
            ),
            ("dashboard/index.html", b"dash".as_slice()),
            ("dashboard/records/_/index.html", b"shell".as_slice()),
            (
                "_next/static/app.js",
                b"fetch(\"/__HAPPYVIEW_BP__/admin\")".as_slice(),
            ),
            ("logo.png", b"\x89PNG/__HAPPYVIEW_BP__".as_slice()),
        ]))
    }

    async fn body(res: axum::response::Response) -> String {
        use http_body_util::BodyExt;
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
    }

    #[tokio::test]
    async fn serves_a_directory_index_for_a_trailing_slash() {
        let d = EmbeddedDashboard::new(export(), None);
        let res = d.respond("/dashboard/");
        assert_eq!(res.status(), 200);
        assert_eq!(res.headers()["content-type"], "text/html");
        assert_eq!(body(res).await, "dash");
    }

    #[tokio::test]
    async fn redirects_a_directory_without_its_trailing_slash() {
        let res = EmbeddedDashboard::new(export(), None).respond("/dashboard");
        assert_eq!(res.status(), 307);
        assert_eq!(res.headers()["location"], "/dashboard/");
    }

    #[tokio::test]
    async fn falls_back_to_the_dynamic_shell_then_the_root() {
        let d = EmbeddedDashboard::new(export(), None);
        assert_eq!(body(d.respond("/dashboard/records/abc")).await, "shell");
        assert_eq!(
            body(d.respond("/nowhere/at/all")).await,
            "<a href=\"/dashboard/\">root</a>"
        );
    }

    #[tokio::test]
    async fn rewrites_the_base_path_sentinel_in_text_assets() {
        let d = EmbeddedDashboard::new(export(), Some("/hv"));
        assert_eq!(
            body(d.respond("/_next/static/app.js")).await,
            "fetch(\"/hv/admin\")"
        );
        assert_eq!(
            body(d.respond("/")).await,
            "<a href=\"/hv/dashboard/\">root</a>"
        );
    }

    #[test]
    fn the_fallback_candidates_run_from_the_shell_to_the_root() {
        assert_eq!(
            spa_fallbacks("/dashboard/records/abc"),
            ["dashboard/records/_/index.html", "index.html"]
        );
        assert_eq!(spa_fallbacks("/nowhere"), ["index.html"]);
        assert_eq!(spa_fallbacks("/"), ["index.html"]);
    }

    #[tokio::test]
    async fn keeps_the_query_on_the_directory_redirect() {
        let res = EmbeddedDashboard::new(export(), None).respond_to("/dashboard", Some("a=1"));
        assert_eq!(res.headers()["location"], "/dashboard/?a=1");
    }

    #[tokio::test]
    async fn serves_non_utf8_text_untouched() {
        use http_body_util::BodyExt;
        let assets = MapAssets(HashMap::from([(
            "a.js",
            b"\xff/__HAPPYVIEW_BP__".as_slice(),
        )]));
        let res = EmbeddedDashboard::new(assets, Some("/hv")).respond("/a.js");
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"\xff/__HAPPYVIEW_BP__");
    }

    #[tokio::test]
    async fn strips_the_sentinel_when_there_is_no_base_path() {
        let d = EmbeddedDashboard::new(export(), None);
        assert_eq!(
            body(d.respond("/_next/static/app.js")).await,
            "fetch(\"/admin\")"
        );
    }

    #[tokio::test]
    async fn leaves_binary_assets_untouched() {
        use http_body_util::BodyExt;
        let res = EmbeddedDashboard::new(export(), Some("/hv")).respond("/logo.png");
        assert_eq!(res.headers()["content-type"], "image/png");
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"\x89PNG/__HAPPYVIEW_BP__");
    }

    /// Only meaningful in a build that embedded a real export: an empty
    /// `web/out` compiles, and this is what notices.
    #[cfg(feature = "embedded-dashboard")]
    #[test]
    fn the_embedded_copy_holds_the_dashboard() {
        use embedded::{Export, RustEmbedAssets};

        // A frontend built without `NEXT_PUBLIC_BASE_PATH=/__HAPPYVIEW_BP__`
        // embeds fine and then ignores the configured base path.
        let has_sentinel = Export::iter().any(|name| {
            (name.ends_with(".html") || name.ends_with(".js"))
                && RustEmbedAssets.get(&name).is_some_and(|bytes| {
                    String::from_utf8_lossy(&bytes).contains(BASE_PATH_SENTINEL)
                })
        });
        assert!(
            has_sentinel,
            "web/out was not exported with the base path sentinel"
        );
        assert!(embedded::RustEmbedAssets.get("index.html").is_some());
        assert!(
            embedded::RustEmbedAssets
                .get("dashboard/index.html")
                .is_some()
        );
    }
}
