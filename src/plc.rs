//! PLC directory I/O.
//!
//! Building, signing, and deriving identifiers for did:plc operations lives in
//! the `happyview-plc` crate, which is I/O-free. What stays here is the part
//! that needs this service's HTTP client and its key-at-rest encryption: talking
//! to a directory, and unwrapping the rotation key we stored for the instance.

use crate::error::AppError;
use base64::Engine;

impl From<happyview_plc::PlcError> for AppError {
    fn from(err: happyview_plc::PlcError) -> Self {
        AppError::Internal(err.to_string())
    }
}

/// Submit a signed PLC operation (genesis or update) to the PLC directory.
///
/// POST `{plc_url}/{did}` with the signed operation as JSON body.
pub async fn submit_operation(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
    signed_op: &serde_json::Value,
) -> Result<(), AppError> {
    let url = format!("{}/{}", plc_url.trim_end_matches('/'), did);
    let resp = http
        .post(&url)
        .json(signed_op)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("PLC submission failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Internal(format!(
            "PLC directory returned {status}: {body}"
        )));
    }
    Ok(())
}

/// Backwards-compatible alias for `submit_operation`.
pub async fn submit_genesis(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
    signed_op: &serde_json::Value,
) -> Result<(), AppError> {
    submit_operation(http, plc_url, did, signed_op).await
}

/// Fetch the last PLC audit log entry for a DID.
///
/// GET `{plc_url}/{did}/log/last` returns the last operation with a `cid` field.
pub async fn fetch_last_operation(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
) -> Result<serde_json::Value, AppError> {
    let url = format!("{}/{}/log/last", plc_url.trim_end_matches('/'), did);
    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch PLC log: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Internal(format!(
            "PLC directory returned {status} for log/last: {body}"
        )));
    }

    resp.json()
        .await
        .map_err(|e| AppError::Internal(format!("failed to parse PLC log: {e}")))
}

/// Decrypt an encrypted key from the database and return the raw bytes.
pub fn decrypt_key(enc_b64: &str, encryption_key: &[u8; 32]) -> Result<Vec<u8>, AppError> {
    let encrypted = base64::engine::general_purpose::STANDARD
        .decode(enc_b64)
        .map_err(|e| AppError::Internal(format!("failed to decode key: {e}")))?;

    crate::plugin::encryption::decrypt(encryption_key, &encrypted)
        .map_err(|e| AppError::Internal(format!("failed to decrypt key: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decrypt_key_invalid_base64() {
        let encryption_key = [0x42u8; 32];
        let result = decrypt_key("not valid base64!!!", &encryption_key);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("decode"),
            "error should mention decoding: {msg}"
        );
    }

    #[test]
    fn decrypt_key_wrong_encryption_key() {
        let correct_key = [0x42u8; 32];
        let wrong_key = [0x99u8; 32];

        let plaintext = [0xAAu8; 32];
        let encrypted = crate::plugin::encryption::encrypt(&correct_key, &plaintext).unwrap();
        let enc_b64 = base64::engine::general_purpose::STANDARD.encode(&encrypted);

        let result = decrypt_key(&enc_b64, &wrong_key);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("decrypt"),
            "error should mention decryption: {msg}"
        );
    }

    /// The crate's errors have to keep reaching callers as `AppError`, since
    /// every handler that builds an operation propagates them with `?`.
    #[test]
    fn plc_crate_errors_convert_to_app_error() {
        let err: AppError = happyview_plc::PlcError::MissingCid.into();
        let msg = format!("{err}");
        assert!(
            msg.contains("CID"),
            "conversion should preserve text: {msg}"
        );
    }
}
