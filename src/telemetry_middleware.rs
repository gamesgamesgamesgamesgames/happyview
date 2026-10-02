//! Bandwidth counters for HTTP traffic HappyView itself serves.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::body::Body;
use axum::extract::{FromRef, Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use bytes::Buf;
use http_body_util::BodyExt;

use crate::AppState;
use crate::telemetry::counters::Counters;

#[derive(Clone)]
pub struct TelemetryState {
    pub counters: Arc<Counters>,
    pub base_path: Option<String>,
}

impl FromRef<AppState> for TelemetryState {
    fn from_ref(state: &AppState) -> Self {
        TelemetryState {
            counters: state.telemetry_counters.clone(),
            base_path: state.config.base_path.clone(),
        }
    }
}

fn cookie_header_has(cookie_header: &str, name: &str) -> bool {
    cookie_header.split(';').any(|part| {
        part.trim()
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with('='))
    })
}

fn strip_base_path<'a>(path: &'a str, base_path: Option<&str>) -> &'a str {
    match base_path {
        Some(bp) => path.strip_prefix(bp).unwrap_or(path),
        None => path,
    }
}

fn count_body_bytes<F>(body: Body, mut on_bytes: F) -> Body
where
    F: FnMut(u64) + Send + 'static,
{
    Body::new(body.map_frame(move |frame| {
        if let Some(data) = frame.data_ref() {
            on_bytes(data.remaining() as u64);
        }
        frame
    }))
}

pub async fn count_http_bytes(
    State(TelemetryState {
        counters,
        base_path,
    }): State<TelemetryState>,
    req: Request,
    next: Next,
) -> Response {
    if strip_base_path(req.uri().path(), base_path.as_deref()).starts_with("/xrpc/") {
        counters.xrpc_requests.fetch_add(1, Ordering::Relaxed);
        let has_credentials = req.headers().contains_key(header::AUTHORIZATION)
            || req
                .headers()
                .get(header::COOKIE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| cookie_header_has(v, crate::auth::COOKIE_NAME));
        if has_credentials {
            counters
                .xrpc_requests_with_credentials
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    let counters_in = counters.clone();
    let req = req.map(|body| {
        count_body_bytes(body, move |n| {
            counters_in.http_bytes_in.fetch_add(n, Ordering::Relaxed);
        })
    });

    let response = next.run(req).await;

    response.map(|body| {
        count_body_bytes(body, move |n| {
            counters.http_bytes_out.fetch_add(n, Ordering::Relaxed);
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::{get, post};
    use tower::ServiceExt;

    fn state(counters: &Arc<Counters>) -> TelemetryState {
        TelemetryState {
            counters: counters.clone(),
            base_path: None,
        }
    }

    #[tokio::test]
    async fn increments_http_bytes_out_by_exactly_the_response_body_length() {
        let counters = Arc::new(Counters::new());
        let body = "x".repeat(1234);

        let app = Router::new()
            .route("/probe", get(move || async move { body }))
            .layer(axum::middleware::from_fn_with_state(
                state(&counters),
                count_http_bytes,
            ))
            .with_state(state(&counters));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let _ = response.into_body().collect().await.unwrap();

        assert_eq!(counters.http_bytes_out.load(Ordering::Relaxed), 1234);
        assert_eq!(counters.http_bytes_in.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn increments_http_bytes_in_by_exactly_the_request_body_length() {
        let counters = Arc::new(Counters::new());
        let payload = "y".repeat(777);

        let app = Router::new()
            .route(
                "/probe",
                post(|body: axum::body::Bytes| async move { body.len().to_string() }),
            )
            .layer(axum::middleware::from_fn_with_state(
                state(&counters),
                count_http_bytes,
            ))
            .with_state(state(&counters));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/probe")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        let _ = response.into_body().collect().await.unwrap();

        assert_eq!(counters.http_bytes_in.load(Ordering::Relaxed), 777);
    }

    #[tokio::test]
    async fn xrpc_path_increments_xrpc_requests_but_a_non_xrpc_path_does_not() {
        let counters = Arc::new(Counters::new());
        let app = Router::new()
            .route("/xrpc/com.example.probe", get(|| async { "ok" }))
            .route("/probe", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                state(&counters),
                count_http_bytes,
            ))
            .with_state(state(&counters));

        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(counters.xrpc_requests.load(Ordering::Relaxed), 0);

        let _ = app
            .oneshot(
                Request::builder()
                    .uri("/xrpc/com.example.probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(counters.xrpc_requests.load(Ordering::Relaxed), 1);
        assert_eq!(
            counters
                .xrpc_requests_with_credentials
                .load(Ordering::Relaxed),
            0
        );
    }

    #[tokio::test]
    async fn xrpc_request_with_authorization_header_increments_both_counters() {
        let counters = Arc::new(Counters::new());
        let app = Router::new()
            .route("/xrpc/com.example.probe", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                state(&counters),
                count_http_bytes,
            ))
            .with_state(state(&counters));

        let _ = app
            .oneshot(
                Request::builder()
                    .uri("/xrpc/com.example.probe")
                    .header("authorization", "Bearer abc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(counters.xrpc_requests.load(Ordering::Relaxed), 1);
        assert_eq!(
            counters
                .xrpc_requests_with_credentials
                .load(Ordering::Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn xrpc_request_with_session_cookie_increments_both_counters() {
        let counters = Arc::new(Counters::new());
        let app = Router::new()
            .route("/xrpc/com.example.probe", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                state(&counters),
                count_http_bytes,
            ))
            .with_state(state(&counters));

        let _ = app
            .oneshot(
                Request::builder()
                    .uri("/xrpc/com.example.probe")
                    .header("cookie", format!("{}=abc123", crate::auth::COOKIE_NAME))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(counters.xrpc_requests.load(Ordering::Relaxed), 1);
        assert_eq!(
            counters
                .xrpc_requests_with_credentials
                .load(Ordering::Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn xrpc_request_under_a_configured_base_path_still_increments_xrpc_requests() {
        let counters = Arc::new(Counters::new());
        let telemetry_state = TelemetryState {
            counters: counters.clone(),
            base_path: Some("/happyview".to_string()),
        };
        let app = Router::new()
            .route("/happyview/xrpc/com.example.probe", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                telemetry_state.clone(),
                count_http_bytes,
            ))
            .with_state(telemetry_state);

        let _ = app
            .oneshot(
                Request::builder()
                    .uri("/happyview/xrpc/com.example.probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(counters.xrpc_requests.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn strip_base_path_only_strips_a_genuine_prefix_match() {
        assert_eq!(
            strip_base_path("/happyview/xrpc/foo", Some("/happyview")),
            "/xrpc/foo"
        );
        assert_eq!(strip_base_path("/xrpc/foo", None), "/xrpc/foo");
        assert_eq!(strip_base_path("/health", Some("/happyview")), "/health");
    }

    #[test]
    fn an_unrelated_route_that_merely_starts_with_the_base_path_string_is_not_misclassified() {
        let stripped = strip_base_path("/application/xrpc-lookalike/foo", Some("/app"));
        assert_eq!(stripped, "lication/xrpc-lookalike/foo");
        assert!(!stripped.starts_with("/xrpc/"));
    }

    #[test]
    fn cookie_header_has_matches_the_exact_cookie_name_only() {
        assert!(cookie_header_has(
            "happyview_session=abc; other=1",
            "happyview_session"
        ));
        assert!(cookie_header_has(
            "other=1; happyview_session=abc",
            "happyview_session"
        ));
        assert!(!cookie_header_has(
            "happyview_session_extra=abc",
            "happyview_session"
        ));
        assert!(!cookie_header_has("other=1", "happyview_session"));
    }
}
