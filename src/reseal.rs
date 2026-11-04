//! Re-sealing stored values under the current `TOKEN_ENCRYPTION_KEY`, the
//! second half of a key rotation, and counting what still needs it.
//!
//! [`STORES`] lists every column that holds a value sealed with the key.
//! Values carry no key id, so a value is classified by trying the keys:
//! one that opens under `TOKEN_ENCRYPTION_KEY_PREVIOUS` but not the current
//! key still needs re-sealing, and the previous key can go only once no store
//! has any. A store with an `encrypted` flag can also hold plaintext from
//! before the instance had a key; those rows are sealed too, but they don't
//! depend on the previous key.
//!
//! Counts are always read from the stored values, never kept: a status can't
//! report a rotation finished while a value still needs the previous key.
//!
//! Every write is a compare-and-set on the values that were read, so a token
//! refresh or a save that lands mid-pass wins, and its value, sealed under the
//! current key already, is left alone.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Serialize;
use sqlx::{AnyPool, Row};

use crate::db::{DatabaseBackend, adapt_sql};
use crate::error::AppError;
use crate::plugin::encryption::{Keyring, Opened, encrypt};

/// How a column holds its value.
#[derive(Clone, Copy, Debug)]
enum Encoding {
    /// Ciphertext bytes in a BLOB/BYTEA column.
    Raw,
    /// Base64 ciphertext in a TEXT column.
    Base64Text,
    /// Base64 ciphertext as ASCII bytes in a BLOB/BYTEA column.
    Base64Blob,
}

#[derive(Clone, Copy, Debug)]
enum Key {
    Text,
    Int,
}

struct Column {
    name: &'static str,
    encoding: Encoding,
}

/// One table, and the columns in it sealed with the key.
struct Store {
    /// The name the status reports.
    name: &'static str,
    table: &'static str,
    key: &'static str,
    key_kind: Key,
    columns: &'static [Column],
    /// The `encrypted` column, where the table has one. A row with it at 0
    /// holds its columns in plaintext.
    flag: Option<&'static str>,
}

const fn col(name: &'static str, encoding: Encoding) -> Column {
    Column { name, encoding }
}

const STORES: &[Store] = &[
    Store {
        name: "dpop_keys",
        table: "happyview_dpop_keys",
        key: "id",
        key_kind: Key::Text,
        columns: &[col("private_key_enc", Encoding::Raw)],
        flag: None,
    },
    Store {
        name: "dpop_sessions",
        table: "happyview_dpop_sessions",
        key: "id",
        key_kind: Key::Text,
        columns: &[
            col("access_token_enc", Encoding::Raw),
            col("refresh_token_enc", Encoding::Raw),
        ],
        flag: None,
    },
    Store {
        name: "oauth_client_keys",
        table: "happyview_oauth_client_keys",
        key: "kid",
        key_kind: Key::Text,
        columns: &[col("private_jwk", Encoding::Raw)],
        flag: Some("encrypted"),
    },
    Store {
        name: "external_account_tokens",
        table: "happyview_external_account_tokens",
        key: "id",
        key_kind: Key::Text,
        columns: &[
            col("access_token", Encoding::Raw),
            col("refresh_token", Encoding::Raw),
        ],
        flag: None,
    },
    Store {
        name: "service_identity",
        table: "happyview_service_identity",
        key: "id",
        key_kind: Key::Int,
        columns: &[
            col("signing_key_enc", Encoding::Base64Text),
            col("rotation_key_enc", Encoding::Base64Text),
        ],
        flag: None,
    },
    Store {
        name: "verification_methods",
        table: "happyview_verification_methods",
        key: "id",
        key_kind: Key::Text,
        columns: &[col("private_key_enc", Encoding::Base64Blob)],
        flag: None,
    },
    Store {
        name: "space_dids",
        table: "happyview_space_dids",
        key: "id",
        key_kind: Key::Text,
        columns: &[
            col("signing_key_enc", Encoding::Raw),
            col("rotation_key_enc", Encoding::Raw),
        ],
        flag: None,
    },
    Store {
        name: "attestation_key",
        table: "happyview_attestation_key",
        key: "id",
        key_kind: Key::Int,
        columns: &[col("private_key", Encoding::Base64Text)],
        flag: Some("encrypted"),
    },
    Store {
        name: "delegation_key",
        table: "happyview_delegation_key",
        key: "id",
        key_kind: Key::Int,
        columns: &[col("private_key", Encoding::Base64Text)],
        flag: None,
    },
    Store {
        name: "script_variables",
        table: "happyview_script_variables",
        key: "key",
        key_kind: Key::Text,
        columns: &[col("value", Encoding::Base64Text)],
        flag: Some("encrypted"),
    },
    Store {
        name: "oauth_sessions",
        table: crate::auth::oauth_store::SESSIONS_TABLE,
        key: "did",
        key_kind: Key::Text,
        columns: &[col("session_data", Encoding::Base64Text)],
        flag: Some("encrypted"),
    },
    Store {
        name: "linked_repo_sessions",
        table: crate::linked_repos::client::LINKED_SESSIONS_TABLE,
        key: "did",
        key_kind: Key::Text,
        columns: &[col("session_data", Encoding::Base64Text)],
        flag: Some("encrypted"),
    },
    Store {
        name: "oauth_state",
        table: "happyview_oauth_state",
        key: "state_key",
        key_kind: Key::Text,
        columns: &[col("state_data", Encoding::Base64Text)],
        flag: Some("encrypted"),
    },
];

/// Plugin secrets are base64 values inside `happyview_plugin_configs.config`.
const PLUGIN_SECRETS: &str = "plugin_secrets";

/// Rows read and re-sealed at a time.
const BATCH: i64 = 200;

/// What one store holds that a rotation still has to deal with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StoreCount {
    pub store: &'static str,
    /// Rows holding a value only the previous key opens.
    pub previous: u64,
    /// Rows stored in plaintext, from before the instance had a key.
    pub unsealed: u64,
    /// Rows holding a value neither key opens. Nothing can recover these.
    pub unreadable: u64,
}

/// What a pass changed, per store.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StoreReseal {
    pub store: &'static str,
    /// Rows re-sealed from the previous key, or sealed from plaintext.
    pub resealed: u64,
    /// Rows that changed after they were read, and were left to the writer.
    pub changed: u64,
    pub unreadable: u64,
}

/// A stored value, as it is bound back for the compare-and-set.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Stored {
    Bytes(Vec<u8>),
    Text(String),
}

#[derive(Clone, Debug)]
enum KeyValue {
    Text(String),
    Int(i64),
}

/// What a value needs.
enum Need {
    /// Sealed under the current key, or empty.
    Nothing,
    /// Re-seal this plaintext under the current key.
    Seal(Vec<u8>),
    Unreadable,
}

struct Fetched {
    key: KeyValue,
    values: Vec<Option<Stored>>,
    flag: Option<i64>,
}

fn ciphertext(encoding: Encoding, stored: &Stored) -> Option<Vec<u8>> {
    match (encoding, stored) {
        (Encoding::Raw, Stored::Bytes(b)) => Some(b.clone()),
        (Encoding::Base64Text, Stored::Text(t)) => BASE64.decode(t).ok(),
        (Encoding::Base64Blob, Stored::Bytes(b)) => BASE64.decode(b).ok(),
        _ => None,
    }
}

fn plaintext(encoding: Encoding, stored: &Stored) -> Vec<u8> {
    match (encoding, stored) {
        (_, Stored::Bytes(b)) => b.clone(),
        (_, Stored::Text(t)) => t.as_bytes().to_vec(),
    }
}

fn need(keys: &Keyring, encoding: Encoding, stored: &Stored, sealed: bool) -> Need {
    if !sealed {
        return Need::Seal(plaintext(encoding, stored));
    }
    match ciphertext(encoding, stored).map(|ct| keys.which_opens(&ct)) {
        Some(Opened::Current(_)) => Need::Nothing,
        Some(Opened::Previous(plain)) => Need::Seal(plain),
        Some(Opened::Neither) | None => Need::Unreadable,
    }
}

fn encode(keys: &Keyring, encoding: Encoding, plain: &[u8]) -> Result<Stored, AppError> {
    let sealed = encrypt(keys.current(), plain)
        .map_err(|e| AppError::Internal(format!("failed to seal a value: {e}")))?;
    Ok(match encoding {
        Encoding::Raw => Stored::Bytes(sealed),
        Encoding::Base64Text => Stored::Text(BASE64.encode(sealed)),
        Encoding::Base64Blob => Stored::Bytes(BASE64.encode(sealed).into_bytes()),
    })
}

fn read_value(
    row: &sqlx::any::AnyRow,
    i: usize,
    encoding: Encoding,
) -> Result<Option<Stored>, sqlx::Error> {
    Ok(match encoding {
        Encoding::Base64Text => row.try_get::<Option<String>, _>(i)?.map(Stored::Text),
        Encoding::Raw | Encoding::Base64Blob => {
            row.try_get::<Option<Vec<u8>>, _>(i)?.map(Stored::Bytes)
        }
    })
}

async fn page(
    pool: &AnyPool,
    backend: DatabaseBackend,
    store: &Store,
    after: Option<&KeyValue>,
) -> Result<Vec<Fetched>, AppError> {
    let key = match store.key_kind {
        Key::Text => store.key.to_string(),
        Key::Int => format!("CAST({} AS BIGINT)", store.key),
    };
    let mut select = vec![key];
    select.extend(store.columns.iter().map(|c| c.name.to_string()));
    if let Some(flag) = store.flag {
        select.push(format!("CAST({flag} AS BIGINT)"));
    }
    let filter = if after.is_some() {
        format!(" WHERE {} > ?", store.key)
    } else {
        String::new()
    };
    let sql = adapt_sql(
        &format!(
            "SELECT {} FROM {}{filter} ORDER BY {} LIMIT ?",
            select.join(", "),
            store.table,
            store.key
        ),
        backend,
    );
    let mut query = crate::db::query(&sql);
    match after {
        Some(KeyValue::Text(k)) => query = query.bind(k.clone()),
        Some(KeyValue::Int(k)) => query = query.bind(*k),
        None => {}
    }
    let rows = query
        .bind(BATCH)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read {}: {e}", store.table)))?;

    let read = |row: &sqlx::any::AnyRow| -> Result<Fetched, sqlx::Error> {
        let key = match store.key_kind {
            Key::Text => KeyValue::Text(row.try_get(0)?),
            Key::Int => KeyValue::Int(row.try_get(0)?),
        };
        let values = store
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| read_value(row, i + 1, c.encoding))
            .collect::<Result<_, _>>()?;
        let flag = match store.flag {
            Some(_) => Some(row.try_get::<i64, _>(store.columns.len() + 1)?),
            None => None,
        };
        Ok(Fetched { key, values, flag })
    };
    rows.iter()
        .map(read)
        .collect::<Result<_, _>>()
        .map_err(|e| AppError::Internal(format!("failed to read {}: {e}", store.table)))
}

/// What a row needs, value by value.
fn needs(keys: &Keyring, store: &Store, row: &Fetched) -> Vec<Need> {
    let sealed = row.flag != Some(0);
    store
        .columns
        .iter()
        .zip(&row.values)
        .map(|(c, v)| match v {
            None => Need::Nothing,
            Some(v) => need(keys, c.encoding, v, sealed),
        })
        .collect()
}

fn count_row(count: &mut StoreCount, row: &Fetched, needs: &[Need]) {
    if needs.iter().any(|n| matches!(n, Need::Unreadable)) {
        count.unreadable += 1;
    }
    if needs.iter().any(|n| matches!(n, Need::Seal(_))) {
        if row.flag == Some(0) {
            count.unsealed += 1;
        } else {
            count.previous += 1;
        }
    }
}

/// Write `sealed` over the row, only while every value still holds what was
/// read. Returns whether the row was written.
async fn compare_and_set(
    pool: &AnyPool,
    backend: DatabaseBackend,
    store: &Store,
    row: &Fetched,
    sealed: &[Option<Stored>],
) -> Result<bool, AppError> {
    let mut sets = Vec::new();
    let mut binds: Vec<Stored> = Vec::new();
    for (c, new) in store.columns.iter().zip(sealed) {
        if let Some(new) = new {
            sets.push(format!("{} = ?", c.name));
            binds.push(new.clone());
        }
    }
    if let Some(flag) = store.flag {
        sets.push(format!("{flag} = 1"));
    }
    let mut wheres = vec![format!("{} = ?", store.key)];
    let mut where_binds: Vec<Stored> = Vec::new();
    for (c, old) in store.columns.iter().zip(&row.values) {
        match old {
            Some(old) => {
                wheres.push(format!("{} = ?", c.name));
                where_binds.push(old.clone());
            }
            None => wheres.push(format!("{} IS NULL", c.name)),
        }
    }
    if let (Some(flag), Some(value)) = (store.flag, row.flag) {
        wheres.push(format!("{flag} = {value}"));
    }
    let sql = adapt_sql(
        &format!(
            "UPDATE {} SET {} WHERE {}",
            store.table,
            sets.join(", "),
            wheres.join(" AND ")
        ),
        backend,
    );
    let mut query = crate::db::query(&sql);
    for value in binds {
        query = match value {
            Stored::Bytes(b) => query.bind(b),
            Stored::Text(t) => query.bind(t),
        };
    }
    query = match &row.key {
        KeyValue::Text(k) => query.bind(k.clone()),
        KeyValue::Int(k) => query.bind(*k),
    };
    for value in where_binds {
        query = match value {
            Stored::Bytes(b) => query.bind(b),
            Stored::Text(t) => query.bind(t),
        };
    }
    let result = query
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to re-seal {}: {e}", store.table)))?;
    Ok(result.rows_affected() > 0)
}

/// Count, store by store, what a rotation still has to deal with.
pub async fn status(
    pool: &AnyPool,
    backend: DatabaseBackend,
    keys: &Keyring,
) -> Result<Vec<StoreCount>, AppError> {
    let mut out = Vec::new();
    for store in STORES {
        let mut count = StoreCount {
            store: store.name,
            ..Default::default()
        };
        let mut after = None;
        loop {
            let rows = page(pool, backend, store, after.as_ref()).await?;
            for row in &rows {
                count_row(&mut count, row, &needs(keys, store, row));
            }
            match rows.last() {
                Some(last) if rows.len() as i64 == BATCH => after = Some(last.key.clone()),
                _ => break,
            }
        }
        out.push(count);
    }
    out.push(plugin_secrets(pool, backend, keys, false).await?.0);
    Ok(out)
}

/// Re-seal one batch of every store, from the start each time: a value
/// already under the current key is skipped, so a pass that was interrupted
/// simply runs again. `should_stop` is asked between batches.
pub async fn reseal<F, Fut>(
    pool: &AnyPool,
    backend: DatabaseBackend,
    keys: &Keyring,
    mut should_stop: F,
) -> Result<(Vec<StoreReseal>, Option<&'static str>), AppError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<&'static str>>,
{
    let mut out = Vec::new();
    for store in STORES {
        let mut done = StoreReseal {
            store: store.name,
            ..Default::default()
        };
        let mut after = None;
        loop {
            if let Some(reason) = should_stop().await {
                out.push(done);
                return Ok((out, Some(reason)));
            }
            let rows = page(pool, backend, store, after.as_ref()).await?;
            for row in &rows {
                let needs = needs(keys, store, row);
                if needs.iter().any(|n| matches!(n, Need::Unreadable)) {
                    done.unreadable += 1;
                }
                if !needs.iter().any(|n| matches!(n, Need::Seal(_))) {
                    continue;
                }
                let sealed = store
                    .columns
                    .iter()
                    .zip(&needs)
                    .map(|(c, n)| match n {
                        Need::Seal(plain) => encode(keys, c.encoding, plain).map(Some),
                        _ => Ok(None),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if compare_and_set(pool, backend, store, row, &sealed).await? {
                    done.resealed += 1;
                } else {
                    done.changed += 1;
                }
            }
            match rows.last() {
                Some(last) if rows.len() as i64 == BATCH => after = Some(last.key.clone()),
                _ => break,
            }
        }
        out.push(done);
    }
    if let Some(reason) = should_stop().await {
        return Ok((out, Some(reason)));
    }
    out.push(plugin_secrets(pool, backend, keys, true).await?.1);
    Ok((out, None))
}

/// Count, and when `write` is set re-seal, the secrets in every plugin's
/// config. A config row is one compare-and-set on its whole text.
async fn plugin_secrets(
    pool: &AnyPool,
    backend: DatabaseBackend,
    keys: &Keyring,
    write: bool,
) -> Result<(StoreCount, StoreReseal), AppError> {
    let mut count = StoreCount {
        store: PLUGIN_SECRETS,
        ..Default::default()
    };
    let mut done = StoreReseal {
        store: PLUGIN_SECRETS,
        ..Default::default()
    };
    let rows: Vec<(String, String)> = crate::db::query_as(&adapt_sql(
        "SELECT plugin_id, config FROM happyview_plugin_configs ORDER BY plugin_id",
        backend,
    ))
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(format!("failed to read plugin configs: {e}")))?;

    for (plugin_id, text) in rows {
        let Ok(mut config) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(secrets) = config.get_mut("secrets").and_then(|s| s.as_object_mut()) else {
            continue;
        };
        let (mut previous, mut unreadable) = (false, false);
        for value in secrets.values_mut() {
            let Some(stored) = value.as_str() else {
                continue;
            };
            match need(
                keys,
                Encoding::Base64Text,
                &Stored::Text(stored.to_string()),
                true,
            ) {
                Need::Nothing => {}
                Need::Unreadable => unreadable = true,
                Need::Seal(plain) => {
                    previous = true;
                    if let Stored::Text(t) = encode(keys, Encoding::Base64Text, &plain)? {
                        *value = serde_json::Value::String(t);
                    }
                }
            }
        }
        count.previous += u64::from(previous);
        count.unreadable += u64::from(unreadable);
        done.unreadable += u64::from(unreadable);
        if !(write && previous) {
            continue;
        }
        let result = crate::db::query(&adapt_sql(
            "UPDATE happyview_plugin_configs SET config = ? WHERE plugin_id = ? AND config = ?",
            backend,
        ))
        .bind(config.to_string())
        .bind(&plugin_id)
        .bind(&text)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to re-seal plugin secrets: {e}")))?;
        if result.rows_affected() > 0 {
            done.resealed += 1;
        } else {
            done.changed += 1;
        }
    }
    Ok((count, done))
}

/// At boot, start a re-seal if anything needs one: a rotation under way, or
/// rows stored in plaintext before the instance had a key. Runs beside boot,
/// since counting reads every sealed value.
pub fn spawn_boot_check(state: crate::AppState) {
    let Some(keys) = state.config.keyring() else {
        return;
    };
    tokio::spawn(async move {
        let stores = match status(&state.db, state.db_backend, &keys).await {
            Ok(stores) => stores,
            Err(e) => {
                tracing::warn!(error = %e, "could not count values to re-seal; will retry on next startup");
                return;
            }
        };
        let (previous, unsealed, unreadable) = stores.iter().fold((0, 0, 0), |acc, s| {
            (acc.0 + s.previous, acc.1 + s.unsealed, acc.2 + s.unreadable)
        });
        if unreadable > 0 {
            tracing::warn!(
                unreadable,
                "stored values open under no configured TOKEN_ENCRYPTION_KEY and cannot be re-sealed"
            );
        }
        if previous + unsealed == 0 {
            return;
        }
        match crate::jobs::native::reseal::start(&state, crate::jobs::native::reseal::BOOT_ACTOR)
            .await
        {
            Ok(job_id) => tracing::info!(job_id, previous, unsealed, "re-sealing stored values"),
            Err(e) => {
                tracing::warn!(error = %e, "could not start re-sealing; will retry on next startup")
            }
        }
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Read `store`'s first row, run `between`, then write the re-sealed row:
    /// what a pass does when a writer lands between its read and its write.
    pub(crate) async fn reseal_with_a_write_between<F, Fut>(
        pool: &AnyPool,
        backend: DatabaseBackend,
        keys: &Keyring,
        store: &str,
        between: F,
    ) -> bool
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let store = STORES.iter().find(|s| s.name == store).unwrap();
        let rows = page(pool, backend, store, None).await.unwrap();
        let row = &rows[0];
        let sealed = store
            .columns
            .iter()
            .zip(needs(keys, store, row))
            .map(|(c, n)| match n {
                Need::Seal(plain) => Some(encode(keys, c.encoding, &plain).unwrap()),
                _ => None,
            })
            .collect::<Vec<_>>();
        between().await;
        compare_and_set(pool, backend, store, row, &sealed)
            .await
            .unwrap()
    }

    /// Every column the status counts is one a store's code seals: a table
    /// added to the list by mistake, or misspelt, fails here on both backends.
    #[tokio::test]
    async fn every_listed_store_can_be_read() {
        for (pool, backend) in crate::test_support::test_pools().await {
            for store in STORES {
                page(&pool, backend, store, None).await.unwrap();
            }
        }
    }
}
