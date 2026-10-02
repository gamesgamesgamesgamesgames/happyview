//! Decide whether an API client is a confidential OAuth client by reading the
//! metadata document it publishes.
//!
//! There is deliberately no column an operator can set. The authorization
//! server reads this same document; deriving from it is the only way the two
//! cannot disagree.

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;

/// How long a probe result is trusted before being re-fetched.
pub const PROBE_TTL_SECS: i64 = 60;

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub confidential: bool,
    pub reason: String,
    pub checked_at: String,
}

pub fn evaluate_metadata(
    doc: &serde_json::Value,
    expected_jwks_uri: &str,
    held_kids: &[String],
) -> ProbeResult {
    let checked_at = now_rfc3339();
    let no = |reason: String| ProbeResult {
        confidential: false,
        reason,
        checked_at: checked_at.clone(),
    };

    let method_value = &doc["token_endpoint_auth_method"];
    if let Some(method) = method_value.as_str() {
        if method != "private_key_jwt" {
            return no(format!(
                "published token_endpoint_auth_method is \"{method}\", not \"private_key_jwt\""
            ));
        }
    } else if method_value.is_null() {
        return no(
            "published document does not include token_endpoint_auth_method; expected \"private_key_jwt\""
                .to_string(),
        );
    } else {
        return no(format!(
            "published token_endpoint_auth_method is {method_value}, not \"private_key_jwt\""
        ));
    }

    let Some(jwks_uri) = doc["jwks_uri"].as_str() else {
        if doc.get("jwks").is_some() {
            return no(
                "published document provides jwks inline instead of jwks_uri; the app holds its own key, so HappyView cannot sign on its behalf — publish jwks_uri instead"
                    .to_string(),
            );
        }
        return no(
            "published document has no jwks_uri; HappyView can only sign for a client whose jwks_uri points at it"
                .to_string(),
        );
    };

    if jwks_uri.trim_end_matches('/') != expected_jwks_uri.trim_end_matches('/') {
        return no(format!(
            "published jwks_uri is \"{jwks_uri}\", expected \"{expected_jwks_uri}\""
        ));
    }

    if held_kids.is_empty() {
        return no(
            "published document declares private_key_jwt but HappyView holds no authentication key for this client"
                .to_string(),
        );
    }

    ProbeResult {
        confidential: true,
        reason: "published document declares private_key_jwt against this instance".to_string(),
        checked_at,
    }
}

/// Fetch the client's published document and record the verdict.
pub async fn probe(
    state: &AppState,
    api_client_id: &str,
    client_id_url: &str,
) -> Result<ProbeResult, AppError> {
    let expected = crate::admin::api_clients::jwks_uri_for(state, api_client_id);

    let held: Vec<String> = super::client_keys::load_keys(
        &state.db,
        state.db_backend,
        state.config.token_encryption_key.as_ref(),
        api_client_id,
    )
    .await?
    .into_iter()
    .map(|k| k.kid)
    .collect();

    let result = match state.http.get(client_id_url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(doc) => evaluate_metadata(&doc, &expected, &held),
            Err(e) => ProbeResult {
                confidential: false,
                reason: format!("published document is not valid JSON: {e}"),
                checked_at: now_rfc3339(),
            },
        },
        Ok(resp) => ProbeResult {
            confidential: false,
            reason: format!(
                "fetching the published document returned HTTP {}",
                resp.status()
            ),
            checked_at: now_rfc3339(),
        },
        Err(e) => ProbeResult {
            confidential: false,
            reason: format!("could not fetch the published document: {e}"),
            checked_at: now_rfc3339(),
        },
    };

    let sql = adapt_sql(
        "INSERT INTO happyview_api_client_probes (api_client_id, confidential, reason, checked_at) VALUES (?, ?, ?, ?) ON CONFLICT (api_client_id) DO UPDATE SET confidential = EXCLUDED.confidential, reason = EXCLUDED.reason, checked_at = EXCLUDED.checked_at",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(api_client_id)
        .bind(i32::from(result.confidential))
        .bind(&result.reason)
        .bind(&result.checked_at)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to cache probe result: {e}")))?;

    Ok(result)
}

/// Probe result, re-fetching only when the cached one is older than the TTL.
pub async fn cached(
    state: &AppState,
    api_client_id: &str,
    client_id_url: &str,
) -> Result<ProbeResult, AppError> {
    let sql = adapt_sql(
        "SELECT confidential, reason, checked_at FROM happyview_api_client_probes WHERE api_client_id = ?",
        state.db_backend,
    );
    let row: Option<(i32, String, String)> = crate::db::query_as(&sql)
        .bind(api_client_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read probe cache: {e}")))?;

    if let Some((confidential, reason, checked_at)) = row
        && let Ok(when) = chrono::DateTime::parse_from_rfc3339(&checked_at)
        && (chrono::Utc::now() - when.with_timezone(&chrono::Utc)).num_seconds() < PROBE_TTL_SECS
    {
        return Ok(ProbeResult {
            confidential: confidential != 0,
            reason,
            checked_at,
        });
    }

    probe(state, api_client_id, client_id_url).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const JWKS: &str = "https://hv.example.com/oauth/clients/abc/jwks.json";

    fn kids() -> Vec<String> {
        vec!["kid-1".to_string()]
    }

    #[test]
    fn a_public_document_is_not_confidential() {
        let doc = serde_json::json!({ "token_endpoint_auth_method": "none" });
        let r = evaluate_metadata(&doc, JWKS, &kids());
        assert!(!r.confidential);
        assert!(r.reason.contains("none"));
    }

    #[test]
    fn a_matching_document_is_confidential() {
        let doc = serde_json::json!({
            "token_endpoint_auth_method": "private_key_jwt",
            "token_endpoint_auth_signing_alg": "ES256",
            "jwks_uri": JWKS,
        });
        assert!(evaluate_metadata(&doc, JWKS, &kids()).confidential);
    }

    #[test]
    fn a_jwks_uri_pointing_elsewhere_is_not_confidential() {
        let doc = serde_json::json!({
            "token_endpoint_auth_method": "private_key_jwt",
            "jwks_uri": "https://somewhere.else/jwks.json",
        });
        let r = evaluate_metadata(&doc, JWKS, &kids());
        assert!(!r.confidential);
        assert!(r.reason.contains("jwks_uri"));
    }

    #[test]
    fn declaring_private_key_jwt_with_no_key_held_is_not_confidential() {
        let doc = serde_json::json!({
            "token_endpoint_auth_method": "private_key_jwt",
            "jwks_uri": JWKS,
        });
        let r = evaluate_metadata(&doc, JWKS, &[]);
        assert!(!r.confidential);
        assert!(r.reason.contains("no authentication key"));
    }

    #[test]
    fn a_non_string_token_endpoint_auth_method_reports_what_was_published() {
        let doc = serde_json::json!({ "token_endpoint_auth_method": 42 });
        let r = evaluate_metadata(&doc, JWKS, &kids());
        assert!(!r.confidential);
        assert!(r.reason.contains("42"));
        assert!(!r.reason.contains("none"));
    }

    #[test]
    fn no_jwks_uri_and_no_inline_jwks_is_not_confidential() {
        let doc = serde_json::json!({
            "token_endpoint_auth_method": "private_key_jwt",
        });
        let r = evaluate_metadata(&doc, JWKS, &kids());
        assert!(!r.confidential);
        assert!(r.reason.contains("has no jwks_uri"));
    }

    #[test]
    fn an_inline_jwks_is_not_confidential_for_our_purposes() {
        let doc = serde_json::json!({
            "token_endpoint_auth_method": "private_key_jwt",
            "jwks": { "keys": [] },
        });
        let r = evaluate_metadata(&doc, JWKS, &kids());
        assert!(!r.confidential);
        assert!(r.reason.contains("jwks_uri"));
    }
}
