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
            let path = req.uri().path();
            let segments: Vec<&str> = path.trim_matches('/').split('/').collect();

            if segments.len() >= 2 {
                let parent = segments[..segments.len() - 1].join("/");
                let dynamic_path = format!("{}/{}/_/index.html", dir, parent);
                if let Ok(body) = tokio::fs::read(&dynamic_path).await {
                    return Ok::<_, Infallible>(
                        axum::http::Response::builder()
                            .header("content-type", "text/html; charset=utf-8")
                            .body(Full::new(Bytes::from(body)))
                            .unwrap(),
                    );
                }
            }

            let index = format!("{}/index.html", dir);
            let body = tokio::fs::read(&index).await.unwrap_or_default();
            Ok::<_, Infallible>(
                axum::http::Response::builder()
                    .header("content-type", "text/html; charset=utf-8")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap(),
            )
        }
    });
    router.fallback_service(ServeDir::new(&static_dir).not_found_service(spa_fallback))
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

#[cfg(feature = "embedded-dashboard")]
mod embedded {
    use axum::Router;

    pub(super) fn attach<S: Clone + Send + Sync + 'static>(
        _router: Router<S>,
        _base_path: Option<&str>,
    ) -> Router<S> {
        unimplemented!("the embedded dashboard source is not built yet")
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
}
