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
}

impl DbSessionStore {
    pub fn new(pool: AnyPool, backend: DatabaseBackend) -> Self {
        Self { pool, backend }
    }
}

impl Store<Did, Session> for DbSessionStore {
    type Error = StoreError;

    async fn get(&self, key: &Did) -> Result<Option<Session>, Self::Error> {
        let row: Option<(String,)> = sqlx::query_as(&adapt_sql(
            "SELECT session_data FROM happyview_oauth_sessions WHERE did = ?",
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
        sqlx::query(&adapt_sql(
            "INSERT INTO happyview_oauth_sessions (did, session_data, updated_at) VALUES (?, ?, datetime('now'))
             ON CONFLICT (did) DO UPDATE SET session_data = EXCLUDED.session_data, updated_at = datetime('now')",
            self.backend,
        ))
        .bind(key.as_ref())
        .bind(&json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn del(&self, key: &Did) -> Result<(), Self::Error> {
        sqlx::query(&adapt_sql(
            "DELETE FROM happyview_oauth_sessions WHERE did = ?",
            self.backend,
        ))
        .bind(key.as_ref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn clear(&self) -> Result<(), Self::Error> {
        sqlx::query("DELETE FROM happyview_oauth_sessions")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

impl SessionStore for DbSessionStore {}

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
        let row: Option<(String,)> = sqlx::query_as(&adapt_sql(
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
        sqlx::query(&adapt_sql(
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
        sqlx::query(&adapt_sql(
            "DELETE FROM happyview_oauth_state WHERE state_key = ?",
            self.backend,
        ))
        .bind(key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn clear(&self) -> Result<(), Self::Error> {
        sqlx::query("DELETE FROM happyview_oauth_state")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

impl StateStore for DbStateStore {}
