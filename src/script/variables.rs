//! Script variables at rest: how a value is sealed and opened, and the one
//! place a row's `encrypted` flag is interpreted.
//!
//! A value is AES-256-GCM under `TOKEN_ENCRYPTION_KEY` when the instance has
//! that key. The key is optional, so an instance without one stores and serves
//! plaintext rather than refusing, and [`encrypt_stored`] seals those rows at
//! the first boot that has a key. A row that won't open under the current key,
//! because the key changed, is kept as it is: its value can't be recovered any
//! other way, and setting the variable again replaces it.

use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};

use crate::db::{DatabaseBackend, adapt_sql};
use crate::plugin::encryption::{EncryptionError, decrypt, encrypt};

/// What every variable shows in place of its value. Fixed, so it gives away
/// neither the value nor its length.
pub const MASK: &str = "****";

/// A value as it goes in the table.
#[derive(Debug, PartialEq, Eq)]
pub struct Sealed {
    pub value: String,
    pub encrypted: i32,
}

pub fn seal(key: Option<&[u8; 32]>, value: &str) -> Result<Sealed, EncryptionError> {
    Ok(match key {
        Some(key) => Sealed {
            value: BASE64.encode(encrypt(key, value.as_bytes())?),
            encrypted: 1,
        },
        None => Sealed {
            value: value.to_string(),
            encrypted: 0,
        },
    })
}

/// The value of a row, or `None` if it's ciphertext this instance can't open.
pub fn open(key: Option<&[u8; 32]>, stored: &str, encrypted: bool) -> Option<String> {
    if !encrypted {
        return Some(stored.to_string());
    }
    let bytes = BASE64.decode(stored).ok()?;
    String::from_utf8(decrypt(key?, &bytes).ok()?).ok()
}

/// Every variable a script can read. One that won't open is left out, as an
/// unset variable would be; [`encrypt_stored`] reports it at boot and the
/// dashboard marks it, since a warning here would repeat on every run.
pub async fn load(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    key: Option<&[u8; 32]>,
) -> BTreeMap<String, String> {
    let sql = adapt_sql(
        "SELECT key, value, CAST(encrypted AS BIGINT) FROM happyview_script_variables",
        backend,
    );
    crate::db::query_as::<(String, String, i64)>(&sql)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(name, stored, encrypted)| Some((name, open(key, &stored, encrypted != 0)?)))
        .collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct EncryptReport {
    /// Rows sealed by this run.
    pub encrypted: usize,
    /// Rows left in plaintext: there's no key, or sealing one failed.
    pub plaintext: usize,
    /// Rows that won't open under the current key.
    pub unreadable: usize,
}

/// Seal every plaintext row, and count what can't be read. Safe to run on every
/// boot and from several instances at once: a row is only updated if it's still
/// plaintext with the value that was read, so a save in between wins and no row
/// is sealed twice. A failure leaves the row readable for the next boot.
pub async fn encrypt_stored(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    key: Option<&[u8; 32]>,
) -> Result<EncryptReport, sqlx::Error> {
    let select = adapt_sql(
        "SELECT key, value, CAST(encrypted AS BIGINT) FROM happyview_script_variables",
        backend,
    );
    let rows: Vec<(String, String, i64)> = crate::db::query_as(&select).fetch_all(pool).await?;

    let mut report = EncryptReport::default();
    for (name, stored, encrypted) in rows {
        if encrypted != 0 {
            if open(key, &stored, true).is_none() {
                report.unreadable += 1;
            }
            continue;
        }
        let Some(key) = key else {
            report.plaintext += 1;
            continue;
        };
        let Ok(sealed) = seal(Some(key), &stored) else {
            report.plaintext += 1;
            continue;
        };
        let updated = store_sealed(pool, backend, &name, &stored, &sealed.value).await;
        match updated {
            Ok(result) if result.rows_affected() > 0 => report.encrypted += 1,
            // Changed since it was read, so whatever wrote it has sealed it.
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(variable = %name, error = %e, "could not encrypt a script variable");
                report.plaintext += 1;
            }
        }
    }
    Ok(report)
}

/// Replace a plaintext row with its sealed value, unless it has changed since
/// `read` was read.
async fn store_sealed(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    name: &str,
    read: &str,
    sealed: &str,
) -> Result<sqlx::any::AnyQueryResult, sqlx::Error> {
    let sql = adapt_sql(
        "UPDATE happyview_script_variables SET value = ?, encrypted = 1 \
         WHERE key = ? AND encrypted = 0 AND value = ?",
        backend,
    );
    crate::db::query(&sql)
        .bind(sealed)
        .bind(name)
        .bind(read)
        .execute(pool)
        .await
}

/// [`encrypt_stored`] at boot. Nothing it finds stops an instance starting.
pub async fn encrypt_at_boot(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    key: Option<&[u8; 32]>,
) {
    let report = match encrypt_stored(pool, backend, key).await {
        Ok(report) => report,
        Err(e) => {
            tracing::error!(error = %e, "script variable encryption failed; will retry on next startup");
            return;
        }
    };
    if report.encrypted > 0 {
        tracing::info!(
            count = report.encrypted,
            "encrypted stored script variables"
        );
    }
    if report.plaintext > 0 {
        tracing::warn!(
            count = report.plaintext,
            "script variables are stored unencrypted; set TOKEN_ENCRYPTION_KEY to encrypt them"
        );
    }
    if report.unreadable > 0 {
        tracing::warn!(
            count = report.unreadable,
            "script variables can't be read with this TOKEN_ENCRYPTION_KEY and are withheld from scripts; set them again"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::migrated_memory_pool;

    const KEY: [u8; 32] = [7; 32];
    const OTHER_KEY: [u8; 32] = [9; 32];
    const BACKEND: DatabaseBackend = DatabaseBackend::Sqlite;

    async fn pool_with_plaintext(name: &str, value: &str) -> sqlx::AnyPool {
        let pool = migrated_memory_pool().await;
        crate::db::query("INSERT INTO happyview_script_variables (key, value) VALUES (?, ?)")
            .bind(name)
            .bind(value)
            .execute(&pool)
            .await
            .expect("seed a variable");
        pool
    }

    async fn stored(pool: &sqlx::AnyPool, name: &str) -> (String, i64) {
        crate::db::query_as(
            "SELECT value, CAST(encrypted AS BIGINT) FROM happyview_script_variables WHERE key = ?",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("read a variable")
    }

    #[test]
    fn a_sealed_value_opens_only_under_its_key() {
        let sealed = seal(Some(&KEY), "secret").unwrap();
        assert_eq!(sealed.encrypted, 1);
        assert!(!sealed.value.contains("secret"));
        assert_eq!(
            open(Some(&KEY), &sealed.value, true).as_deref(),
            Some("secret")
        );
        assert_eq!(open(Some(&OTHER_KEY), &sealed.value, true), None);
        assert_eq!(open(None, &sealed.value, true), None);
    }

    #[test]
    fn an_empty_value_and_a_multibyte_one_survive_sealing() {
        for value in ["", "héllo ✨"] {
            let sealed = seal(Some(&KEY), value).unwrap();
            assert_eq!(
                open(Some(&KEY), &sealed.value, true).as_deref(),
                Some(value)
            );
        }
    }

    /// Text that looks like ciphertext is still plaintext: the flag decides.
    #[test]
    fn the_flag_decides_what_a_row_holds_not_its_text() {
        let lookalike = seal(Some(&KEY), "x").unwrap().value;
        assert_eq!(
            open(Some(&KEY), &lookalike, false).as_deref(),
            Some(lookalike.as_str())
        );
        assert_eq!(
            seal(None, "plain").unwrap(),
            Sealed {
                value: "plain".into(),
                encrypted: 0
            }
        );
    }

    #[tokio::test]
    async fn boot_seals_plaintext_and_scripts_still_read_it() {
        let pool = pool_with_plaintext("API_KEY", "secret").await;

        let report = encrypt_stored(&pool, BACKEND, Some(&KEY)).await.unwrap();

        assert_eq!(
            report,
            EncryptReport {
                encrypted: 1,
                ..Default::default()
            }
        );
        let (value, flag) = stored(&pool, "API_KEY").await;
        assert_eq!(flag, 1);
        assert!(!value.contains("secret"));
        assert_eq!(load(&pool, BACKEND, Some(&KEY)).await["API_KEY"], "secret");
    }

    #[tokio::test]
    async fn a_second_run_changes_nothing() {
        let pool = pool_with_plaintext("API_KEY", "secret").await;
        encrypt_stored(&pool, BACKEND, Some(&KEY)).await.unwrap();
        let before = stored(&pool, "API_KEY").await;

        let report = encrypt_stored(&pool, BACKEND, Some(&KEY)).await.unwrap();

        assert_eq!(report, EncryptReport::default());
        assert_eq!(stored(&pool, "API_KEY").await, before);
    }

    #[tokio::test]
    async fn without_a_key_values_stay_readable_and_a_later_key_seals_them() {
        let pool = pool_with_plaintext("API_KEY", "secret").await;

        let report = encrypt_stored(&pool, BACKEND, None).await.unwrap();

        assert_eq!(
            report,
            EncryptReport {
                plaintext: 1,
                ..Default::default()
            }
        );
        assert_eq!(stored(&pool, "API_KEY").await, ("secret".to_string(), 0));
        assert_eq!(load(&pool, BACKEND, None).await["API_KEY"], "secret");

        encrypt_stored(&pool, BACKEND, Some(&KEY)).await.unwrap();
        assert_eq!(load(&pool, BACKEND, Some(&KEY)).await["API_KEY"], "secret");
    }

    /// A changed or missing key can't recover a value, so the row has to
    /// survive for the old key to come back, and scripts see the variable as
    /// unset rather than reading ciphertext.
    #[tokio::test]
    async fn a_value_the_key_cannot_open_is_kept_and_withheld() {
        let pool = pool_with_plaintext("API_KEY", "secret").await;
        encrypt_stored(&pool, BACKEND, Some(&KEY)).await.unwrap();
        let before = stored(&pool, "API_KEY").await;

        for key in [Some(&OTHER_KEY), None] {
            let report = encrypt_stored(&pool, BACKEND, key).await.unwrap();
            assert_eq!(
                report,
                EncryptReport {
                    unreadable: 1,
                    ..Default::default()
                }
            );
            assert!(load(&pool, BACKEND, key).await.is_empty());
            assert_eq!(stored(&pool, "API_KEY").await, before);
        }
        assert_eq!(load(&pool, BACKEND, Some(&KEY)).await["API_KEY"], "secret");
    }

    /// A save landing between the read and the update must win: sealing the
    /// value that was read would put the old one back.
    #[tokio::test]
    async fn sealing_refuses_a_row_that_changed_since_it_was_read() {
        let pool = pool_with_plaintext("API_KEY", "new").await;
        let sealed_old = seal(Some(&KEY), "old").unwrap();

        let result = store_sealed(&pool, BACKEND, "API_KEY", "old", &sealed_old.value)
            .await
            .unwrap();

        assert_eq!(result.rows_affected(), 0);
        assert_eq!(stored(&pool, "API_KEY").await, ("new".to_string(), 0));
    }
}
