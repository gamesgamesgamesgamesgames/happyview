use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Domain {
    pub id: String,
    pub url: String,
    pub is_primary: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl Domain {
    pub fn host(&self) -> &str {
        let after_scheme = self
            .url
            .strip_prefix("https://")
            .or_else(|| self.url.strip_prefix("http://"))
            .unwrap_or(&self.url);
        after_scheme.split(':').next().unwrap_or(after_scheme)
    }
}

/// Makes `public_url` the primary domain at startup.
///
/// With no domains, `public_url` is seeded as the primary. When the primary
/// differs from `public_url` and a domain with that URL already exists, that
/// domain becomes the primary; renaming the old primary to it instead would
/// break the URL's uniqueness. Only when no such domain exists is the primary
/// renamed to `public_url`.
pub async fn sync_primary(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    public_url: &str,
) -> Result<(), sqlx::Error> {
    use crate::db::{adapt_sql, now_rfc3339, query, query_as};

    let now = now_rfc3339();
    let mut tx = crate::db::begin_write(db, backend).await?;
    let rows: Vec<(String, String, i32)> = query_as(&adapt_sql(
        "SELECT id, url, is_primary FROM happyview_domains",
        backend,
    ))
    .fetch_all(&mut *tx)
    .await?;

    if rows.is_empty() {
        query(&adapt_sql(
            "INSERT INTO happyview_domains (id, url, is_primary, created_at, updated_at) VALUES (?, ?, 1, ?, ?)",
            backend,
        ))
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(public_url)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tracing::info!("Seeded primary domain: {public_url}");
        return tx.commit().await;
    }

    let Some((primary_id, primary_url, _)) = rows.iter().find(|(_, _, p)| *p != 0) else {
        return tx.commit().await;
    };
    if primary_url == public_url {
        return tx.commit().await;
    }

    if let Some((id, _, _)) = rows.iter().find(|(_, url, _)| url == public_url) {
        query(&adapt_sql(
            "UPDATE happyview_domains SET is_primary = 0, updated_at = ? WHERE is_primary = 1",
            backend,
        ))
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        query(&adapt_sql(
            "UPDATE happyview_domains SET is_primary = 1, updated_at = ? WHERE id = ?",
            backend,
        ))
        .bind(&now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tracing::info!("Made {public_url} the primary domain in place of {primary_url}");
    } else {
        query(&adapt_sql(
            "UPDATE happyview_domains SET url = ?, updated_at = ? WHERE id = ?",
            backend,
        ))
        .bind(public_url)
        .bind(&now)
        .bind(primary_id)
        .execute(&mut *tx)
        .await?;
        tracing::info!("Updated primary domain URL from {primary_url} to {public_url}");
    }
    tx.commit().await
}

#[derive(Clone)]
pub struct DomainCache {
    by_host: Arc<RwLock<HashMap<String, Arc<Domain>>>>,
    primary: Arc<RwLock<Option<Arc<Domain>>>>,
}

impl DomainCache {
    pub fn new() -> Self {
        Self {
            by_host: Arc::new(RwLock::new(HashMap::new())),
            primary: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn load(&self, domains: Vec<Domain>) {
        let mut by_host = self.by_host.write().await;
        let mut primary = self.primary.write().await;

        by_host.clear();
        *primary = None;

        for domain in domains {
            let arc = Arc::new(domain);
            if arc.is_primary {
                *primary = Some(arc.clone());
            }
            by_host.insert(arc.host().to_string(), arc);
        }
    }

    pub async fn get(&self, host: &str) -> Option<Arc<Domain>> {
        let by_host = self.by_host.read().await;
        by_host.get(host).cloned()
    }

    pub async fn primary(&self) -> Option<Arc<Domain>> {
        let primary = self.primary.read().await;
        primary.clone()
    }

    pub async fn insert(&self, domain: Domain) {
        let arc = Arc::new(domain);
        let mut by_host = self.by_host.write().await;
        let mut primary = self.primary.write().await;

        if arc.is_primary {
            *primary = Some(arc.clone());
        }
        by_host.insert(arc.host().to_string(), arc);
    }

    pub async fn remove(&self, host: &str) {
        let mut by_host = self.by_host.write().await;
        let removed = by_host.remove(host);

        if let Some(domain) = removed
            && domain.is_primary
        {
            let mut primary = self.primary.write().await;
            *primary = None;
        }
    }

    pub async fn set_primary(&self, host: &str) {
        let by_host = self.by_host.read().await;
        if let Some(domain) = by_host.get(host).cloned() {
            drop(by_host);
            let mut primary = self.primary.write().await;
            *primary = Some(domain);
        }
    }

    pub async fn all(&self) -> Vec<Arc<Domain>> {
        let by_host = self.by_host.read().await;
        by_host.values().cloned().collect()
    }

    /// Return `true` if `origin` (a browser `Origin` header value, e.g.
    /// `https://example.com` or `http://localhost:3000`) exactly matches a
    /// registered domain's URL.
    ///
    /// This is the trusted-origin allowlist for credentialed (cookie-bearing)
    /// CORS: only first-party domains HappyView actually serves may send
    /// credentials cross-origin. The match is on the full origin
    /// (scheme + host + port), not just the host, so `http` and `https` or a
    /// different port are treated as distinct origins.
    pub async fn is_allowed_origin(&self, origin: &str) -> bool {
        let target = origin.trim_end_matches('/');
        let by_host = self.by_host.read().await;
        by_host
            .values()
            .any(|d| d.url.trim_end_matches('/') == target)
    }
}

impl Default for DomainCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_domain(url: &str, is_primary: bool) -> Domain {
        Domain {
            id: Uuid::new_v4().to_string(),
            url: url.to_string(),
            is_primary,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn host_strips_https() {
        let domain = make_domain("https://example.com", false);
        assert_eq!(domain.host(), "example.com");
    }

    #[test]
    fn host_strips_http() {
        let domain = make_domain("http://localhost:3000", false);
        assert_eq!(domain.host(), "localhost");
    }

    #[tokio::test]
    async fn load_and_get() {
        let cache = DomainCache::new();
        let domains = vec![
            make_domain("https://example.com", true),
            make_domain("https://other.com", false),
        ];
        cache.load(domains).await;

        let found = cache.get("example.com").await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().url, "https://example.com");

        assert!(cache.get("other.com").await.is_some());

        let missing = cache.get("unknown.com").await;
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn primary_returns_primary_domain() {
        let cache = DomainCache::new();
        let domains = vec![
            make_domain("https://example.com", false),
            make_domain("https://primary.com", true),
        ];
        cache.load(domains).await;

        let primary = cache.primary().await;
        assert!(primary.is_some());
        assert_eq!(primary.unwrap().url, "https://primary.com");
    }

    #[tokio::test]
    async fn insert_and_remove() {
        let cache = DomainCache::new();
        let domain = make_domain("https://example.com", false);
        cache.insert(domain).await;

        assert!(cache.get("example.com").await.is_some());

        cache.remove("example.com").await;
        assert!(cache.get("example.com").await.is_none());
    }

    #[tokio::test]
    async fn is_allowed_origin_matches_full_origin() {
        let cache = DomainCache::new();
        cache
            .load(vec![
                make_domain("https://example.com", true),
                make_domain("http://localhost:3000", false),
            ])
            .await;

        // Exact matches (trailing slash tolerated).
        assert!(cache.is_allowed_origin("https://example.com").await);
        assert!(cache.is_allowed_origin("https://example.com/").await);
        assert!(cache.is_allowed_origin("http://localhost:3000").await);

        // Scheme, port, and host must all match.
        assert!(!cache.is_allowed_origin("http://example.com").await);
        assert!(!cache.is_allowed_origin("https://example.com:8443").await);
        assert!(!cache.is_allowed_origin("http://localhost:3001").await);
        assert!(!cache.is_allowed_origin("https://evil.example").await);
    }

    #[tokio::test]
    async fn set_primary_updates() {
        let cache = DomainCache::new();
        let domains = vec![
            make_domain("https://example.com", true),
            make_domain("https://other.com", false),
        ];
        cache.load(domains).await;

        // Initially example.com is primary
        assert_eq!(cache.primary().await.unwrap().url, "https://example.com");

        // Change primary to other.com
        cache.set_primary("other.com").await;
        assert_eq!(cache.primary().await.unwrap().url, "https://other.com");
    }
}
