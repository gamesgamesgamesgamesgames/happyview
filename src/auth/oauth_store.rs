use atrium_api::types::string::Did;
use atrium_common::store::Store;
use atrium_oauth::store::session::{Session, SessionStore};
use atrium_oauth::store::state::{InternalStateData, StateStore};
use sqlx::AnyPool;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::db::{DatabaseBackend, adapt_sql};
use crate::plugin::encryption::{EncryptionError, open, seal};

#[derive(Debug)]
pub enum StoreError {
    Sqlx(sqlx::Error),
    Json(serde_json::Error),
    Seal(EncryptionError),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Sqlx(e) => write!(f, "database error: {e}"),
            StoreError::Json(e) => write!(f, "json error: {e}"),
            StoreError::Seal(e) => write!(f, "encryption error: {e}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Sqlx(e) => Some(e),
            StoreError::Json(e) => Some(e),
            StoreError::Seal(e) => Some(e),
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

pub const SESSIONS_TABLE: &str = "happyview_oauth_sessions";

/// A stored session's JSON, for callers that read a field without restoring the
/// session. `None` when there's no row, or it won't open under this key, which
/// a caller treats the same way: nothing known, and the user signs in again.
pub async fn stored_session_json(
    pool: &AnyPool,
    backend: DatabaseBackend,
    table: &str,
    did: &str,
    key: Option<&[u8; 32]>,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String, i64)> = crate::db::query_as(&adapt_sql(
        &format!("SELECT session_data, CAST(encrypted AS BIGINT) FROM {table} WHERE did = ?"),
        backend,
    ))
    .bind(did)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(data, encrypted)| open(key, &data, encrypted != 0)))
}

/// Encrypt every plaintext session. Safe on every boot and from several
/// instances: a row is only updated if it's still plaintext with the value that
/// was read, so a refresh in between wins. A row that fails stays readable and
/// is retried at the next boot. Returns how many rows were sealed.
pub async fn seal_stored_sessions(
    pool: &AnyPool,
    backend: DatabaseBackend,
    key: &[u8; 32],
) -> Result<usize, sqlx::Error> {
    let mut sealed_rows = 0;
    for table in [
        SESSIONS_TABLE,
        crate::linked_repos::client::LINKED_SESSIONS_TABLE,
    ] {
        let rows: Vec<(String, String)> = crate::db::query_as(&adapt_sql(
            &format!("SELECT did, session_data FROM {table} WHERE encrypted = 0"),
            backend,
        ))
        .fetch_all(pool)
        .await?;
        let update = adapt_sql(
            &format!(
                "UPDATE {table} SET session_data = ?, encrypted = 1 \
                 WHERE did = ? AND encrypted = 0 AND session_data = ?"
            ),
            backend,
        );
        for (did, data) in rows {
            let Ok(sealed) = seal(Some(key), &data) else {
                continue;
            };
            let result = crate::db::query(&update)
                .bind(&sealed.value)
                .bind(&did)
                .bind(&data)
                .execute(pool)
                .await?;
            sealed_rows += result.rows_affected() as usize;
        }
    }
    Ok(sealed_rows)
}

#[derive(Clone)]
pub struct DbSessionStore {
    pool: AnyPool,
    backend: DatabaseBackend,
    table: &'static str,
    signing_kid: Option<String>,
    encryption_key: Option<[u8; 32]>,
}

impl DbSessionStore {
    pub fn new(pool: AnyPool, backend: DatabaseBackend, key: Option<&[u8; 32]>) -> Self {
        Self::new_with_table(pool, backend, SESSIONS_TABLE, key)
    }

    /// `key` is required rather than set by a builder method, so a store can't
    /// be built that silently writes plaintext.
    pub fn new_with_table(
        pool: AnyPool,
        backend: DatabaseBackend,
        table: &'static str,
        key: Option<&[u8; 32]>,
    ) -> Self {
        Self {
            pool,
            backend,
            table,
            signing_kid: None,
            encryption_key: key.copied(),
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
        let data = stored_session_json(
            &self.pool,
            self.backend,
            self.table,
            key.as_ref(),
            self.encryption_key.as_ref(),
        )
        .await?;
        Ok(data.map(|data| serde_json::from_str(&data)).transpose()?)
    }

    async fn set(&self, key: Did, value: Session) -> Result<(), Self::Error> {
        let json = serde_json::to_string(&value)?;
        let sealed = seal(self.encryption_key.as_ref(), &json).map_err(StoreError::Seal)?;
        // `updated_at` is TEXT, and `adapt_sql` turns `datetime('now')` into
        // Postgres's `NOW()`, whose assignment cast stores a space-separated
        // `+00` form — so the column holds one shape on SQLite and another on
        // Postgres, and neither is the `+00:00` RFC 3339 every other time
        // column here carries. Nothing compares or orders it, so that costs
        // nothing; anything that starts to must bind `now_rfc3339()` here
        // first, since the two shapes do not sort against each other and a
        // SQL clock cannot be compared against TEXT on Postgres at all.
        crate::db::query(&adapt_sql(
            &format!(
                "INSERT INTO {} (did, session_data, encrypted, signing_kid, updated_at) \
                 VALUES (?, ?, ?, ?, datetime('now')) \
                 ON CONFLICT (did) DO UPDATE SET session_data = EXCLUDED.session_data, \
                 encrypted = EXCLUDED.encrypted, updated_at = datetime('now')",
                self.table
            ),
            self.backend,
        ))
        .bind(key.as_ref())
        .bind(&sealed.value)
        .bind(sealed.encrypted)
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
                encrypted INTEGER NOT NULL DEFAULT 0,
                signing_kid TEXT,
                updated_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    const KEY: [u8; 32] = [2; 32];

    async fn raw_row(pool: &AnyPool, did: &str) -> (String, i64) {
        crate::db::query_as(
            "SELECT session_data, CAST(encrypted AS BIGINT) FROM happyview_oauth_sessions WHERE did = ?",
        )
        .bind(did)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_session_is_sealed_on_write_and_opened_on_read() {
        let pool = sessions_pool().await;
        let did = Did::new("did:plc:sealed".to_string()).unwrap();
        let store = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, Some(&KEY));

        store
            .set(did.clone(), sample_session("tok-secret"))
            .await
            .unwrap();

        let (stored, encrypted) = raw_row(&pool, did.as_ref()).await;
        assert_eq!(encrypted, 1);
        assert!(!stored.contains("tok-secret"));
        let session = store.get(&did).await.unwrap().expect("the session opens");
        assert_eq!(session.token_set.access_token, "tok-secret");
    }

    /// A refresh after sealing rewrites the row under the same flag, and a row
    /// from before sealing is still read.
    #[tokio::test]
    async fn a_plaintext_row_stays_readable_and_a_rewrite_seals_it() {
        let pool = sessions_pool().await;
        let did = Did::new("did:plc:plain".to_string()).unwrap();
        DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, None)
            .set(did.clone(), sample_session("tok-old"))
            .await
            .unwrap();

        let store = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, Some(&KEY));
        assert_eq!(
            store
                .get(&did)
                .await
                .unwrap()
                .unwrap()
                .token_set
                .access_token,
            "tok-old"
        );

        store
            .set(did.clone(), sample_session("tok-new"))
            .await
            .unwrap();
        assert_eq!(raw_row(&pool, did.as_ref()).await.1, 1);
    }

    /// Nothing can recover a session this key won't open, but signing in again
    /// replaces it, so it reads as no session rather than as an error.
    #[tokio::test]
    async fn a_session_another_key_sealed_reads_as_none_and_stays_stored() {
        let pool = sessions_pool().await;
        let did = Did::new("did:plc:other".to_string()).unwrap();
        DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, Some(&KEY))
            .set(did.clone(), sample_session("tok"))
            .await
            .unwrap();
        let before = raw_row(&pool, did.as_ref()).await;

        for key in [Some(&[3u8; 32]), None] {
            let store = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, key);
            assert!(store.get(&did).await.unwrap().is_none());
        }
        assert_eq!(raw_row(&pool, did.as_ref()).await, before);
    }

    #[tokio::test]
    async fn boot_seals_both_session_tables_once() {
        let pool = crate::test_support::migrated_memory_pool().await;
        for table in [
            SESSIONS_TABLE,
            crate::linked_repos::client::LINKED_SESSIONS_TABLE,
        ] {
            crate::db::query(&format!(
                "INSERT INTO {table} (did, session_data, updated_at) VALUES ('did:plc:a', '{{\"t\":1}}', 'now')"
            ))
            .execute(&pool)
            .await
            .unwrap();
        }

        let sealed = seal_stored_sessions(&pool, DatabaseBackend::Sqlite, &KEY)
            .await
            .unwrap();
        assert_eq!(sealed, 2);
        assert_eq!(
            seal_stored_sessions(&pool, DatabaseBackend::Sqlite, &KEY)
                .await
                .unwrap(),
            0
        );

        for table in [
            SESSIONS_TABLE,
            crate::linked_repos::client::LINKED_SESSIONS_TABLE,
        ] {
            let json = stored_session_json(
                &pool,
                DatabaseBackend::Sqlite,
                table,
                "did:plc:a",
                Some(&KEY),
            )
            .await
            .unwrap();
            assert_eq!(json.as_deref(), Some("{\"t\":1}"));
        }
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

        let store = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, None)
            .with_signing_kid(Some("kid-1".to_string()));
        store
            .set(did.clone(), sample_session("tok-1"))
            .await
            .unwrap();
        assert_eq!(
            stored_signing_kid(&pool, did.as_ref()).await,
            Some("kid-1".to_string())
        );

        let store_after_rotation = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, None)
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
        let store_k1 = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, None)
            .with_signing_kid(Some("kid-1".to_string()));
        store_k1
            .set(did.clone(), sample_session("tok-1"))
            .await
            .unwrap();
        assert_eq!(
            stored_signing_kid(&pool, did.as_ref()).await,
            Some("kid-1".to_string())
        );

        let store_k2 = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, None)
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

        let store_k3 = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, None)
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

        let store = DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, None);
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
        DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, None)
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
        DbSessionStore::new(pool.clone(), DatabaseBackend::Sqlite, None)
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
