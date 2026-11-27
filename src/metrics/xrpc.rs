//! XRPC requests: how many requests were sent to paths under `/xrpc/`, by
//! method kind, per minute, counted independently of any instance setting.
//! Every such request counts, routed or not, whatever it answers. See
//! [`super::counter`] for how they're kept.

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::AnyPool;

use crate::db::DatabaseBackend;

use super::counter::MinuteCounter;

/// An XRPC requests row's `kind`.
pub const KIND: &str = "xrpc_requests";

/// What a request was: a query is a GET, a procedure a POST, anything else
/// (an OPTIONS preflight, a HEAD) other.
pub const METHODS: [&str; 3] = ["query", "procedure", "other"];

pub struct XrpcRequests(MinuteCounter<{ METHODS.len() }>);

impl Default for XrpcRequests {
    fn default() -> Self {
        Self(MinuteCounter::new(KIND, METHODS))
    }
}

impl std::ops::Deref for XrpcRequests {
    type Target = MinuteCounter<{ METHODS.len() }>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The index in [`METHODS`] of a request with HTTP method `method`.
pub fn method_index(method: &axum::http::Method) -> usize {
    match *method {
        axum::http::Method::GET => 0,
        axum::http::Method::POST => 1,
        _ => 2,
    }
}

impl XrpcRequests {
    /// One request with HTTP method `method` at `at` (seconds).
    pub fn count_at(&self, at: u64, method: &axum::http::Method) {
        self.0.add_at(at, method_index(method));
    }

    /// The requests from `from` up to `to`, read at `now`, as one point per
    /// `width`-second step with any.
    pub async fn read(
        &self,
        pool: &AnyPool,
        backend: DatabaseBackend,
        now: u64,
        from: u64,
        to: u64,
        width: u64,
    ) -> Result<Vec<XrpcPoint>, sqlx::Error> {
        Ok(self
            .0
            .read_steps(pool, backend, now, from, to, width)
            .await?
            .into_iter()
            .map(|(start, counts)| XrpcPoint {
                start: super::rfc3339(start),
                requests: counts.iter().sum(),
                methods: self.0.named(counts),
            })
            .collect())
    }
}

/// The XRPC requests in one step, by method kind.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct XrpcPoint {
    /// RFC 3339, UTC.
    pub start: String,
    pub requests: u64,
    /// Every method kind that saw requests in the step.
    pub methods: BTreeMap<&'static str, u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;

    const T0: u64 = 1_793_318_400;
    const SQLITE: DatabaseBackend = DatabaseBackend::Sqlite;

    #[tokio::test]
    async fn requests_are_counted_by_method_from_the_table_and_memory() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let x = XrpcRequests::default();
        x.count_at(T0 + 1, &Method::GET);
        x.count_at(T0 + 2, &Method::GET);
        x.count_at(T0 + 3, &Method::POST);
        x.write_closed(&pool, SQLITE, "a", T0 + 120).await.unwrap();
        x.count_at(T0 + 130, &Method::OPTIONS);
        let hour = x
            .read(&pool, SQLITE, T0 + 140, T0, T0 + 3_600, 3_600)
            .await
            .unwrap();
        assert_eq!(
            hour,
            vec![XrpcPoint {
                start: super::super::rfc3339(T0),
                requests: 4,
                methods: BTreeMap::from([("other", 1), ("procedure", 1), ("query", 2)]),
            }]
        );
    }

    #[tokio::test]
    async fn requests_and_deliveries_keep_to_their_own_rows() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let x = XrpcRequests::default();
        let d = super::super::delivered::Delivered::default();
        x.count_at(T0, &Method::GET);
        d.count_at(T0, super::super::ingest::RecordOrigin::Live);
        x.write_closed(&pool, SQLITE, "a", T0 + 60).await.unwrap();
        d.write_closed(&pool, SQLITE, "a", T0 + 60).await.unwrap();
        let xs = x
            .read(&pool, SQLITE, T0 + 60, T0, T0 + 60, 60)
            .await
            .unwrap();
        let ds = d
            .read(&pool, SQLITE, T0 + 60, T0, T0 + 60, 60)
            .await
            .unwrap();
        assert_eq!((xs[0].requests, ds[0].records), (1, 1));
    }
}
