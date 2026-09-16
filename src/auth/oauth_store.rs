use atrium_api::types::string::Did;
use atrium_common::store::Store;
use atrium_oauth::store::session::{Session, SessionStore};
use atrium_oauth::store::state::{InternalStateData, StateStore};
use sqlx::AnyPool;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::db::{DatabaseBackend, adapt_sql};

#[derive(Debug)]
pub enum StoreError {
    Sqlx(sqlx::Error),
    Json(serde_json::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Sqlx(e) => write!(f, "database error: {e}"),
            StoreError::Json(e) => write!(f, "json error: {e}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Sqlx(e) => Some(e),
            StoreError::Json(e) => Some(e),
        }
    }
}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        StoreError::Sqlx(e)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Json(e)
    }
}

// --- DbSessionStore ---

#[derive(Clone)]
pub struct DbSessionStore {
    pool: AnyPool,
    backend: DatabaseBackend,
    table: &'static str,
    signing_kid: Option<String>,
}

impl DbSessionStore {
    pub fn new(pool: AnyPool, backend: DatabaseBackend) -> Self {
        Self::new_with_table(pool, backend, "happyview_oauth_sessions")
    }

    pub fn new_with_table(pool: AnyPool, backend: DatabaseBackend, table: &'static str) -> Self {
        Self {
            pool,
            backend,
            table,
            signing_kid: None,
        }
    }

    pub fn with_signing_kid(mut self, kid: Option<String>) -> Self {
        self.signing_kid = kid;
        self
    }
}

impl Store<Did, Session> for DbSessionStore {
    type Error = StoreError;

    async fn get(&self, key: &Did) -> Result<Option<Session>, Self::Error> {
        let row: Option<(String,)> = crate::db::query_as(&adapt_sql(
            &format!("SELECT session_data FROM {} WHERE did = ?", self.table),
            self.backend,
        ))
        .bind(key.as_ref())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some((data,)) => Ok(Some(serde_json::from_str(&data)?)),
            None => Ok(None),
        }
    }

    async fn set(&self, key: Did, value: Session) -> Result<(), Self::Error> {
        let json = serde_json::to_string(&value)?;
        crate::db::query(&adapt_sql(
            &format!(
                "INSERT INTO {} (did, session_data, signing_kid, updated_at) VALUES (?, ?, ?, datetime('now')) \
                 ON CONFLICT (did) DO UPDATE SET session_data = EXCLUDED.session_data, \
                 updated_at = datetime('now')",
                self.table
            ),
            self.backend,
        ))
        .bind(key.as_ref())
        .bind(&json)
        .bind(self.signing_kid.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn del(&self, key: &Did) -> Result<(), Self::Error> {
        crate::db::query(&adapt_sql(
            &format!("DELETE FROM {} WHERE did = ?", self.table),
            self.backend,
        ))
        .bind(key.as_ref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn clear(&self) -> Result<(), Self::Error> {
        crate::db::query(&format!("DELETE FROM {}", self.table))
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

impl SessionStore for DbSessionStore {}

pub async fn lookup_signing_kid(
    pool: &AnyPool,
    backend: DatabaseBackend,
    table: &str,
    did: &str,
) -> Result<Option<String>, StoreError> {
    let row: Option<(Option<String>,)> = crate::db::query_as(&adapt_sql(
        &format!("SELECT signing_kid FROM {table} WHERE did = ?"),
        backend,
    ))
    .bind(did)
    .fetch_optional(pool)
    .await?;

    Ok(row.and_then(|(kid,)| kid))
}

pub async fn stamp_signing_kid_if_unset(
    pool: &AnyPool,
    backend: DatabaseBackend,
    table: &'static str,
    id_column: &'static str,
    id: &str,
    kid: &str,
) -> Result<(), StoreError> {
    crate::db::query(&adapt_sql(
        &format!(
            "UPDATE {table} SET signing_kid = ? WHERE {id_column} = ? AND signing_kid IS NULL"
        ),
        backend,
    ))
    .bind(kid)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Unconditionally overwrite `signing_kid` for a session row — the opposite
/// of [`stamp_signing_kid_if_unset`], which only ever fills a `NULL`.
pub async fn repin_signing_kid(
    pool: &AnyPool,
    backend: DatabaseBackend,
    table: &'static str,
    id_column: &'static str,
    id: &str,
    kid: Option<&str>,
) -> Result<(), StoreError> {
    crate::db::query(&adapt_sql(
        &format!("UPDATE {table} SET signing_kid = ? WHERE {id_column} = ?"),
        backend,
    ))
    .bind(kid)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

// --- DbStateStore ---

#[derive(Clone)]
pub struct DbStateStore {
    pool: AnyPool,
    backend: DatabaseBackend,
    /// Captures the most recently stored state key so callers can associate
    /// additional data (e.g., redirect URIs) with the OAuth state.
    last_state_key: Arc<Mutex<Option<String>>>,
    /// Serializes authorize() + take_last_state_key() pairs so concurrent
    /// logins cannot interleave and swap each other's state keys.
    pub authorize_lock: Arc<tokio::sync::Mutex<()>>,
}

impl DbStateStore {
    pub fn new(pool: AnyPool, backend: DatabaseBackend) -> Self {
        Self {
            pool,
            backend,
            last_state_key: Arc::new(Mutex::new(None)),
            authorize_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Returns the state key from the most recent `set()` call, clearing it.
    pub fn take_last_state_key(&self) -> Option<String> {
        self.last_state_key.lock().unwrap().take()
    }
}

impl Store<String, InternalStateData> for DbStateStore {
    type Error = StoreError;

    async fn get(&self, key: &String) -> Result<Option<InternalStateData>, Self::Error> {
        let row: Option<(String,)> = crate::db::query_as(&adapt_sql(
            "SELECT state_data FROM happyview_oauth_state WHERE state_key = ?",
            self.backend,
        ))
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some((data,)) => Ok(Some(serde_json::from_str(&data)?)),
            None => Ok(None),
        }
    }

    async fn set(&self, key: String, value: InternalStateData) -> Result<(), Self::Error> {
        let json = serde_json::to_string(&value)?;
        crate::db::query(&adapt_sql(
            "INSERT INTO happyview_oauth_state (state_key, state_data) VALUES (?, ?)
             ON CONFLICT (state_key) DO UPDATE SET state_data = EXCLUDED.state_data",
            self.backend,
        ))
        .bind(&key)
        .bind(&json)
        .execute(&self.pool)
        .await?;
        *self.last_state_key.lock().unwrap() = Some(key);
        Ok(())
    }

    async fn del(&self, key: &String) -> Result<(), Self::Error> {
        crate::db::query(&adapt_sql(
            "DELETE FROM happyview_oauth_state WHERE state_key = ?",
            self.backend,
        ))
        .bind(key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn clear(&self) -> Result<(), Self::Error> {
        crate::db::query("DELETE FROM happyview_oauth_state")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

impl StateStore for DbStateStore {}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_oauth::TokenSet;

    async fn sessions_pool() -> AnyPool {
        let pool = crate::test_support::memory_pool().await;
        crate::db::query(
            "CREATE TABLE happyview_oauth_sessions (
                did TEXT PRIMARY KEY,
                session_data TEXT NOT NULL,
                signing_kid TEXT,
                updated_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn sample_session(access_token: &str) -> Session {
        let key = crate::oauth::client_keys::generate_client_key("test-owner").unwrap();
        let dpop_key = crate::oauth::client_keys::to_atrium_jwk(&key).unwrap().key;
        let token_set: TokenSet = serde_json::from_value(serde_json::json!({
            "iss": "https://issuer.example",
            "sub": "did:plc:abc123test",
            "aud": "https://aud.example",
            "scope": null,
            "refresh_token": null,
            "access_token": access_token,
            "token_type": "DPoP",
            "expires_at": null,
        }))
        .unwrap();
        Session {
            dpop_key,
            token_set,
        }
    }

    async fn stored_signing_kid(pool: &AnyPool, did: &str) -> Option<String> {
        let row: (Option<String>,) =
            crate::db::query_as("SELECT signing_kid FROM happyview_oauth_sessions WHERE did = ?")
                .bind(did)
                .fetch_one(pool)
                .await
                .unwrap();
        row.0
    }

    #[tokio::test]
    async fn set_stamps_signing_kid_only_on_the_first_insert() {
        let pool = sessions_pool().await;
        let did = Did::new("did:plc:abc123test".to_string()).unwrap();

        let store = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite)
            .with_signing_kid(Some("kid-1".to_string()));
        store
            .set(did.clone(), sample_session("tok-1"))
            .await
            .unwrap();
        assert_eq!(
            stored_signing_kid(&pool, did.as_ref()).await,
            Some("kid-1".to_string())
        );

        let store_after_rotation = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite)
            .with_signing_kid(Some("kid-2".to_string()));
        store_after_rotation
            .set(did.clone(), sample_session("tok-2"))
            .await
            .unwrap();
        assert_eq!(
            stored_signing_kid(&pool, did.as_ref()).await,
            Some("kid-1".to_string()),
            "signing_kid must not change on a later set()"
        );

        let (session_data,): (String,) =
            crate::db::query_as("SELECT session_data FROM happyview_oauth_sessions WHERE did = ?")
                .bind(did.as_ref())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(session_data.contains("tok-2"));
    }

    #[tokio::test]
    async fn repin_moves_the_pin_but_a_later_refresh_set_does_not() {
        let pool = sessions_pool().await;
        let did = Did::new("did:plc:abc123test".to_string()).unwrap();

        // Establish under kid-1, as an initial login would.
        let store_k1 = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite)
            .with_signing_kid(Some("kid-1".to_string()));
        store_k1
            .set(did.clone(), sample_session("tok-1"))
            .await
            .unwrap();
        assert_eq!(
            stored_signing_kid(&pool, did.as_ref()).await,
            Some("kid-1".to_string())
        );

        let store_k2 = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite)
            .with_signing_kid(Some("kid-2".to_string()));
        store_k2
            .set(did.clone(), sample_session("tok-2"))
            .await
            .unwrap();
        assert_eq!(
            stored_signing_kid(&pool, did.as_ref()).await,
            Some("kid-1".to_string()),
            "atrium's own set() must not move the pin by itself — this is the bug this task fixes downstream"
        );

        repin_signing_kid(
            &pool,
            DatabaseBackend::Sqlite,
            "happyview_oauth_sessions",
            "did",
            did.as_ref(),
            Some("kid-2"),
        )
        .await
        .unwrap();
        assert_eq!(
            stored_signing_kid(&pool, did.as_ref()).await,
            Some("kid-2".to_string()),
            "an unconditional re-pin after a fresh exchange must move the kid"
        );

        let store_k3 = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite)
            .with_signing_kid(Some("kid-3".to_string()));
        store_k3
            .set(did.clone(), sample_session("tok-3"))
            .await
            .unwrap();
        assert_eq!(
            stored_signing_kid(&pool, did.as_ref()).await,
            Some("kid-2".to_string()),
            "a refresh's set() must not move the kid the callback just re-pinned"
        );
    }

    #[tokio::test]
    async fn set_leaves_signing_kid_null_for_an_unpinned_store() {
        let pool = sessions_pool().await;
        let did = Did::new("did:plc:abc123test".to_string()).unwrap();

        let store = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite);
        store
            .set(did.clone(), sample_session("tok-1"))
            .await
            .unwrap();

        assert_eq!(stored_signing_kid(&pool, did.as_ref()).await, None);
    }

    #[tokio::test]
    async fn lookup_signing_kid_distinguishes_no_row_from_no_pin() {
        let pool = sessions_pool().await;

        // No row at all.
        assert_eq!(
            lookup_signing_kid(
                &pool,
                DatabaseBackend::Sqlite,
                "happyview_oauth_sessions",
                "did:plc:missing"
            )
            .await
            .unwrap(),
            None
        );

        // A row that exists but was never pinned.
        let unpinned_did = Did::new("did:plc:unpinnedtest".to_string()).unwrap();
        DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite)
            .set(unpinned_did.clone(), sample_session("tok-1"))
            .await
            .unwrap();
        assert_eq!(
            lookup_signing_kid(
                &pool,
                DatabaseBackend::Sqlite,
                "happyview_oauth_sessions",
                unpinned_did.as_ref()
            )
            .await
            .unwrap(),
            None
        );

        // A row pinned to a kid on its first insert.
        let pinned_did = Did::new("did:plc:pinnedtest".to_string()).unwrap();
        DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite)
            .with_signing_kid(Some("kid-1".to_string()))
            .set(pinned_did.clone(), sample_session("tok-2"))
            .await
            .unwrap();
        assert_eq!(
            lookup_signing_kid(
                &pool,
                DatabaseBackend::Sqlite,
                "happyview_oauth_sessions",
                pinned_did.as_ref()
            )
            .await
            .unwrap(),
            Some("kid-1".to_string())
        );
    }
}
