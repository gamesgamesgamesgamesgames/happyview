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
        match err {
            // An operation over the directory's limits is the request's fault,
            // e.g. an operator adding an eleventh service entry.
            happyview_plc::PlcError::InvalidOperation(_) => AppError::BadRequest(err.to_string()),
            _ => AppError::Internal(err.to_string()),
        }
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
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let body = resp.text().await.unwrap_or_default();
        if let Some(limited) = rate_limit(status, retry_after, &body) {
            return Err(limited);
        }
        return Err(AppError::Internal(format!(
            "PLC directory returned {status}: {body}"
        )));
    }
    Ok(())
}

/// Read a refused submission as a rate limit, when it is one.
///
/// The directory counts operations per DID (10 an hour, 30 a day, 100 a week)
/// and refuses one past a limit with a 400 whose body is only
/// `{"message": "To many operations within last hour (max 10)"}`: the
/// `rate_limit` code it raises internally never reaches the response. Its load
/// balancer answers a 429 of its own. Either way the caller should wait, so
/// both become [`AppError::RateLimited`], with the window the directory named
/// as the wait.
fn rate_limit(
    status: reqwest::StatusCode,
    retry_after: Option<u64>,
    body: &str,
) -> Option<AppError> {
    const HOUR: u64 = 60 * 60;

    let (retry_after, limit) = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        (retry_after.unwrap_or(60), 0)
    } else if status == reqwest::StatusCode::BAD_REQUEST {
        let message = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v["message"].as_str().map(str::to_owned))
            .unwrap_or_default();
        if !message.contains("operations within last") {
            return None;
        }
        if message.contains("last hour") {
            (HOUR, 10)
        } else if message.contains("last day") {
            (24 * HOUR, 30)
        } else if message.contains("last week") {
            (7 * 24 * HOUR, 100)
        } else {
            return None;
        }
    } else {
        return None;
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    Some(AppError::RateLimited {
        retry_after,
        limit,
        reset: now + retry_after,
    })
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
pub fn decrypt_key(
    enc_b64: &str,
    encryption_key: &crate::plugin::encryption::Keyring,
) -> Result<Vec<u8>, AppError> {
    let encrypted = base64::engine::general_purpose::STANDARD
        .decode(enc_b64)
        .map_err(|e| AppError::Internal(format!("failed to decode key: {e}")))?;

    encryption_key
        .decrypt(&encrypted)
        .map_err(|e| AppError::Internal(format!("failed to decrypt key: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decrypt_key_invalid_base64() {
        let encryption_key = [0x42u8; 32];
        let result = decrypt_key(
            "not valid base64!!!",
            &crate::plugin::encryption::Keyring::from(encryption_key),
        );
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

        let result = decrypt_key(
            &enc_b64,
            &crate::plugin::encryption::Keyring::from(wrong_key),
        );
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("decrypt"),
            "error should mention decryption: {msg}"
        );
    }

    fn retry_after_of(err: Option<AppError>) -> Option<(u64, u32)> {
        match err {
            Some(AppError::RateLimited {
                retry_after, limit, ..
            }) => Some((retry_after, limit)),
            _ => None,
        }
    }

    #[test]
    fn the_directorys_per_did_limits_read_as_rate_limits() {
        let bad = reqwest::StatusCode::BAD_REQUEST;
        for (message, expected) in [
            ("To many operations within last hour (max 10)", (3600, 10)),
            ("To many operations within last day (max 30)", (86_400, 30)),
            (
                "To many operations within last week (max 100)",
                (604_800, 100),
            ),
        ] {
            let body = serde_json::json!({ "message": message }).to_string();
            assert_eq!(
                retry_after_of(rate_limit(bad, None, &body)),
                Some(expected),
                "{message}"
            );
        }
    }

    #[test]
    fn a_429_reads_as_a_rate_limit_with_its_retry_after() {
        let status = reqwest::StatusCode::TOO_MANY_REQUESTS;
        assert_eq!(
            retry_after_of(rate_limit(status, Some(30), "")),
            Some((30, 0))
        );
        assert_eq!(retry_after_of(rate_limit(status, None, "")), Some((60, 0)));
    }

    #[test]
    fn other_refusals_are_not_rate_limits() {
        let bad = reqwest::StatusCode::BAD_REQUEST;
        let body = serde_json::json!({ "message": "Invalid signature on op" }).to_string();
        assert!(rate_limit(bad, None, &body).is_none());
        assert!(rate_limit(reqwest::StatusCode::INTERNAL_SERVER_ERROR, None, "").is_none());
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
