use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use sqlx::AnyPool;

use super::{DelegateRole, DelegateView, DelegatedAccountView};

pub async fn create_delegated_account(
    pool: &AnyPool,
    backend: crate::db::DatabaseBackend,
    account_did: &str,
    linked_by: &str,
    api_client_id: &str,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_delegated_accounts (account_did, linked_by, api_client_id, created_at) VALUES (?, ?, ?, ?)",
        backend,
    );
    sqlx::query(&sql)
        .bind(account_did)
        .bind(linked_by)
        .bind(api_client_id)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create delegated account: {e}")))?;
    Ok(())
}

pub async fn delete_delegated_account(
    pool: &AnyPool,
    backend: crate::db::DatabaseBackend,
    account_did: &str,
) -> Result<(), AppError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_delegated_accounts WHERE account_did = ?",
        backend,
    );
    sqlx::query(&sql)
        .bind(account_did)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete delegated account: {e}")))?;
    Ok(())
}

pub async fn get_delegated_account_owner(
    pool: &AnyPool,
    backend: crate::db::DatabaseBackend,
    account_did: &str,
) -> Result<Option<String>, AppError> {
    let sql = adapt_sql(
        "SELECT linked_by FROM happyview_delegated_accounts WHERE account_did = ?",
        backend,
    );
    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind(account_did)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query delegated account: {e}")))?;
    Ok(row.map(|r| r.0))
}

pub async fn is_account_linked(
    pool: &AnyPool,
    backend: crate::db::DatabaseBackend,
    account_did: &str,
) -> Result<bool, AppError> {
    let owner = get_delegated_account_owner(pool, backend, account_did).await?;
    Ok(owner.is_some())
}

pub async fn get_api_client_id(
    pool: &AnyPool,
    backend: crate::db::DatabaseBackend,
    account_did: &str,
) -> Result<Option<String>, AppError> {
    let sql = adapt_sql(
        "SELECT api_client_id FROM happyview_delegated_accounts WHERE account_did = ?",
        backend,
    );
    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind(account_did)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query delegated account: {e}")))?;
    Ok(row.map(|r| r.0))
}

pub async fn add_delegate(
    pool: &AnyPool,
    backend: crate::db::DatabaseBackend,
    account_did: &str,
    user_did: &str,
    role: DelegateRole,
    granted_by: &str,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_account_delegates (account_did, user_did, role, granted_by, created_at) VALUES (?, ?, ?, ?, ?)",
        backend,
    );
    sqlx::query(&sql)
        .bind(account_did)
        .bind(user_did)
        .bind(role.as_str())
        .bind(granted_by)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to add delegate: {e}")))?;
    Ok(())
}

pub async fn remove_delegate(
    pool: &AnyPool,
    backend: crate::db::DatabaseBackend,
    account_did: &str,
    user_did: &str,
) -> Result<(), AppError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_account_delegates WHERE account_did = ? AND user_did = ?",
        backend,
    );
    sqlx::query(&sql)
        .bind(account_did)
        .bind(user_did)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to remove delegate: {e}")))?;
    Ok(())
}

pub async fn get_delegate_role(
    pool: &AnyPool,
    backend: crate::db::DatabaseBackend,
    account_did: &str,
    user_did: &str,
) -> Result<Option<DelegateRole>, AppError> {
    let sql = adapt_sql(
        "SELECT role FROM happyview_account_delegates WHERE account_did = ? AND user_did = ?",
        backend,
    );
    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind(account_did)
        .bind(user_did)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query delegate role: {e}")))?;
    Ok(row.and_then(|r| DelegateRole::from_str(&r.0)))
}

pub async fn list_accounts_for_user(
    pool: &AnyPool,
    backend: crate::db::DatabaseBackend,
    user_did: &str,
    api_client_id: &str,
) -> Result<Vec<DelegatedAccountView>, AppError> {
    let sql = adapt_sql(
        "SELECT ad.account_did, ad.role, ad.created_at FROM happyview_account_delegates ad JOIN happyview_delegated_accounts da ON da.account_did = ad.account_did WHERE ad.user_did = ? AND da.api_client_id = ? ORDER BY ad.created_at DESC",
        backend,
    );
    let rows: Vec<(String, String, String)> = sqlx::query_as(&sql)
        .bind(user_did)
        .bind(api_client_id)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list delegated accounts: {e}")))?;

    Ok(rows
        .into_iter()
        .map(|(did, role, created_at)| DelegatedAccountView {
            did,
            role,
            created_at,
        })
        .collect())
}

pub async fn get_account_for_user(
    pool: &AnyPool,
    backend: crate::db::DatabaseBackend,
    account_did: &str,
    user_did: &str,
) -> Result<Option<(String, String, String)>, AppError> {
    let sql = adapt_sql(
        "SELECT da.linked_by, ad.role, ad.created_at FROM happyview_delegated_accounts da JOIN happyview_account_delegates ad ON da.account_did = ad.account_did WHERE da.account_did = ? AND ad.user_did = ?",
        backend,
    );
    let row: Option<(String, String, String)> = sqlx::query_as(&sql)
        .bind(account_did)
        .bind(user_did)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get delegated account: {e}")))?;
    Ok(row)
}

pub async fn list_delegates(
    pool: &AnyPool,
    backend: crate::db::DatabaseBackend,
    account_did: &str,
) -> Result<Vec<DelegateView>, AppError> {
    let sql = adapt_sql(
        "SELECT user_did, role, granted_by, created_at FROM happyview_account_delegates WHERE account_did = ? ORDER BY created_at ASC",
        backend,
    );
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(&sql)
        .bind(account_did)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list delegates: {e}")))?;

    Ok(rows
        .into_iter()
        .map(|(user_did, role, granted_by, created_at)| DelegateView {
            user_did,
            role,
            granted_by,
            created_at,
        })
        .collect())
}
