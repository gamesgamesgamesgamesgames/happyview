//! What a script's return value becomes on the wire.
//!
//! A method's lexicon declares how its output is encoded, exactly as
//! `com.atproto.sync.getBlob` declares `*/*`, and that declaration — not the
//! shape of whatever the script happened to return — decides whether the
//! answer is JSON or bytes.
//!
//! Gating on the lexicon rather than sniffing the value is the whole design.
//! A method declaring `application/json` is never examined for byte forms, so
//! a response that legitimately contains a blob ref in one of its fields
//! cannot be mistaken for a request to serve that blob.

use std::collections::BTreeSet;

use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::blobs;
use crate::cid_verify::BYTES_B64;
use crate::db::DatabaseBackend;
use crate::error::{AppError, ScriptErrorType};
use crate::lexicon::ParsedLexicon;
use crate::script::encoding::Encoding;

/// What the script asked for, read from its return value and before any
/// storage is touched.
#[derive(Debug, Clone, PartialEq)]
enum Payload {
    /// Bytes already stored, named by CID. Nothing crosses the interpreter
    /// boundary: the script names the content and the host sends it.
    Stored {
        cid: String,
        mime_type: Option<String>,
    },
    /// Bytes the script produced itself, carried as `$bytes`.
    Inline {
        bytes: Vec<u8>,
        mime_type: Option<String>,
    },
}

/// A script's answer, resolved far enough to both log and send.
#[derive(Debug)]
pub enum ScriptResponse {
    Json(Value),
    /// An error the method's lexicon declares. JSON like any other answer, but
    /// at 400: every atproto client decides whether a call failed from the
    /// status before it looks at the body, so a refusal at 200 is read as a
    /// successful response that happens to contain an `error` field.
    Failure(Value),
    Bytes {
        bytes: Vec<u8>,
        content_type: String,
        /// Present when the bytes came from storage, for the event log.
        cid: Option<String>,
    },
}

impl ScriptResponse {
    /// What the event log records. A byte response is summarised rather than
    /// inlined: a megabyte of base64 in `event_logs` is a cost paid on every
    /// request, for a payload nobody reads out of a log row.
    pub fn log_detail(&self) -> Value {
        match self {
            ScriptResponse::Json(value) | ScriptResponse::Failure(value) => value.clone(),
            ScriptResponse::Bytes {
                bytes,
                content_type,
                cid,
            } => json!({ "mimeType": content_type, "size": bytes.len(), "cid": cid }),
        }
    }

    pub fn response_size(&self) -> usize {
        match self {
            ScriptResponse::Json(value) | ScriptResponse::Failure(value) => value.to_string().len(),
            ScriptResponse::Bytes { bytes, .. } => bytes.len(),
        }
    }

    pub fn into_response(self) -> Response {
        match self {
            ScriptResponse::Json(value) => axum::Json(value).into_response(),
            ScriptResponse::Failure(value) => {
                (StatusCode::BAD_REQUEST, axum::Json(value)).into_response()
            }
            ScriptResponse::Bytes {
                bytes,
                content_type,
                ..
            } => ([(CONTENT_TYPE, content_type)], bytes).into_response(),
        }
    }
}

/// Whether a returned value is a script's structured failure rather than an
/// answer: an object carrying a string `error`, and neither of the byte forms.
/// Both tests matter -- a blob ref or a `$bytes` table could carry a field
/// called `error` of its own, and that is a payload, not a refusal.
fn is_failure(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if !object.get("error").is_some_and(Value::is_string) {
        return false;
    }
    object.get("$type").and_then(Value::as_str) != Some("blob") && !object.contains_key("$bytes")
}

/// The error names `defs.main.errors` declares. A refusal is recognised by the
/// lexicon naming it, not by the value's shape alone -- the same reason the
/// encoding decides JSON against bytes.
pub fn declared_errors(lexicon: &ParsedLexicon) -> BTreeSet<String> {
    lexicon
        .raw
        .pointer("/defs/main/errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| error.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a value is a refusal naming an error this method declares.
fn declared_failure(value: &Value, declared: &BTreeSet<String>) -> bool {
    is_failure(value)
        && value
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|name| declared.contains(name))
}

fn refuse(method: &str, message: impl Into<String>) -> AppError {
    AppError::ScriptError {
        error_type: ScriptErrorType::Runtime,
        message: message.into(),
        method: method.to_string(),
        line: None,
    }
}

/// Read a byte-returning script's value as one of the two accepted forms.
fn payload(method: &str, value: &Value) -> Result<Payload, AppError> {
    let object = value.as_object().ok_or_else(|| {
        refuse(
            method,
            "this method's lexicon declares a non-JSON output encoding, so its \
             script must return either a blob ref or a table carrying $bytes",
        )
    })?;

    let mime_type = object
        .get("mimeType")
        .and_then(Value::as_str)
        .map(str::to_string);

    // A blob ref is atproto's own shape, and the `$type` is load-bearing: a
    // bare `$link` is how a record link is spelled too.
    if object.get("$type").and_then(Value::as_str) == Some("blob") {
        let cid = object
            .get("ref")
            .and_then(|link| link.get("$link"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                refuse(
                    method,
                    "the returned blob ref carries no ref.$link naming the blob to serve",
                )
            })?;
        return Ok(Payload::Stored {
            cid: cid.to_string(),
            mime_type,
        });
    }

    if let Some(encoded) = object.get("$bytes").and_then(Value::as_str) {
        let bytes = base64::Engine::decode(&BYTES_B64, encoded)
            .map_err(|_| refuse(method, "the returned $bytes is not valid base64"))?;
        return Ok(Payload::Inline { bytes, mime_type });
    }

    Err(refuse(
        method,
        "this method's lexicon declares a non-JSON output encoding, so its \
         script must return either a blob ref or a table carrying $bytes",
    ))
}

/// Turn a script's return value into the response its lexicon describes.
///
/// Takes a pool rather than the whole `AppState` so the shape rules can be
/// exercised without building one.
pub async fn resolve(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    method: &str,
    encoding: &Encoding,
    declared_errors: &BTreeSet<String>,
    value: Value,
) -> Result<ScriptResponse, AppError> {
    // A refusal answers JSON whatever the success encoding is, which is how
    // `com.atproto.sync.getBlob` behaves: it declares `*/*` and still answers
    // `BlobNotFound` as JSON at 400.
    if declared_failure(&value, declared_errors) {
        return Ok(ScriptResponse::Failure(value));
    }

    let Encoding::Json = encoding else {
        // A refusal the lexicon does not declare still cannot be bytes, so it
        // keeps the 200 it has always answered rather than being refused by
        // the byte forms below: a script's failure is a table, and naming an
        // error the lexicon omits is a reason to fix the lexicon, not to turn
        // the method into a 500.
        if is_failure(&value) {
            return Ok(ScriptResponse::Json(value));
        }

        let payload = payload(method, &value)?;

        let (bytes, stored_type, cid) = match payload {
            Payload::Stored { cid, mime_type } => {
                let blob = blobs::get(pool, backend, &cid).await?.ok_or_else(|| {
                    AppError::NotFound(format!("no blob is stored for cid {cid}"))
                })?;
                (blob.bytes, mime_type.or(Some(blob.mime_type)), Some(cid))
            }
            Payload::Inline { bytes, mime_type } => (bytes, mime_type, None),
        };

        // A pinned encoding is the method's contract, so it wins over whatever
        // the script named; `*/*` delegates the choice and therefore requires
        // one. A missing type is refused rather than defaulted, because a
        // wrong content type on a download surfaces far from its cause.
        let content_type = match encoding {
            Encoding::Fixed(declared) => declared.clone(),
            _ => stored_type.ok_or_else(|| {
                refuse(
                    method,
                    "this method's lexicon declares `*/*`, so its script must \
                     name the content type as mimeType",
                )
            })?,
        };

        return Ok(ScriptResponse::Bytes {
            bytes,
            content_type,
            cid,
        });
    };

    Ok(ScriptResponse::Json(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::migrated_memory_pool;

    const METHOD: &str = "at.example.getThing";

    fn no_errors() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn declaring(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn blob_ref(cid: &str, mime: Option<&str>) -> Value {
        let mut value = json!({ "$type": "blob", "ref": { "$link": cid } });
        if let Some(mime) = mime {
            value["mimeType"] = json!(mime);
        }
        value
    }

    /// The gate itself. A JSON method's value is passed through untouched even
    /// when it carries the very shapes the byte path looks for, so a response
    /// that legitimately embeds a blob ref cannot become a binary download.
    /// Deleting the encoding check makes this fail.
    #[tokio::test]
    async fn a_json_method_is_never_read_as_bytes() {
        let pool = migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;

        for value in [
            blob_ref("bafkreibogus", Some("application/wasm")),
            json!({ "$bytes": "aGk=", "mimeType": "text/plain" }),
            json!({ "avatar": blob_ref("bafkreibogus", Some("image/png")) }),
        ] {
            let answer = resolve(
                &pool,
                backend,
                METHOD,
                &Encoding::Json,
                &no_errors(),
                value.clone(),
            )
            .await
            .expect("a JSON method should answer");
            match answer {
                ScriptResponse::Json(passed) => assert_eq!(passed, value),
                other => panic!("a json-declared method answered {other:?}: {value}"),
            }
        }
    }

    #[tokio::test]
    async fn stored_bytes_are_served_by_reference() {
        let pool = migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        let cid = crate::blobs::put(&pool, backend, b"\x00\x01wasm", "application/wasm")
            .await
            .expect("store");

        let answer = resolve(
            &pool,
            backend,
            METHOD,
            &Encoding::Any,
            &no_errors(),
            blob_ref(&cid, None),
        )
        .await
        .expect("serve");

        match answer {
            ScriptResponse::Bytes {
                bytes,
                content_type,
                cid: served,
            } => {
                assert_eq!(bytes, b"\x00\x01wasm");
                // Named by neither the script nor the lexicon, so it is the
                // type the bytes were stored under.
                assert_eq!(content_type, "application/wasm");
                assert_eq!(served.as_deref(), Some(cid.as_str()));
            }
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn computed_bytes_come_back_from_dollar_bytes() {
        let pool = migrated_memory_pool().await;
        let answer = resolve(
            &pool,
            DatabaseBackend::Sqlite,
            METHOD,
            &Encoding::Any,
            &no_errors(),
            json!({ "$bytes": "aGVsbG8=", "mimeType": "text/plain" }),
        )
        .await
        .expect("serve");

        match answer {
            ScriptResponse::Bytes {
                bytes,
                content_type,
                cid,
            } => {
                assert_eq!(bytes, b"hello");
                assert_eq!(content_type, "text/plain");
                assert!(cid.is_none(), "inline bytes came from no row");
            }
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    /// A pinned encoding is the method's contract, so it decides the header
    /// even when the script names something else.
    #[tokio::test]
    async fn a_pinned_encoding_wins_over_the_scripts_mime_type() {
        let pool = migrated_memory_pool().await;
        let answer = resolve(
            &pool,
            DatabaseBackend::Sqlite,
            METHOD,
            &Encoding::Fixed("application/wasm".into()),
            &no_errors(),
            json!({ "$bytes": "aGk=", "mimeType": "text/plain" }),
        )
        .await
        .expect("serve");

        match answer {
            ScriptResponse::Bytes { content_type, .. } => {
                assert_eq!(content_type, "application/wasm")
            }
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_refusals_name_what_the_script_should_have_returned() {
        let pool = migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;

        let cases: Vec<(Value, &str)> = vec![
            // `*/*` delegates the choice, so it has to be made.
            (json!({ "$bytes": "aGk=" }), "mimeType"),
            (json!({ "$bytes": "not base64!" }), "base64"),
            (json!({ "$type": "blob" }), "ref.$link"),
            (json!("a bare string"), "blob ref"),
            (json!({ "rows": [] }), "blob ref"),
        ];

        for (value, expected) in cases {
            let error = resolve(
                &pool,
                backend,
                METHOD,
                &Encoding::Any,
                &no_errors(),
                value.clone(),
            )
            .await
            .expect_err(&format!("{value} should be refused"));
            match error {
                AppError::ScriptError { message, .. } => assert!(
                    message.contains(expected),
                    "{value} refused with {message:?}, which does not mention {expected:?}"
                ),
                other => panic!("{value} refused as {other:?}, not a script error"),
            }
        }
    }

    #[tokio::test]
    async fn a_cid_that_is_not_stored_is_not_found() {
        let pool = migrated_memory_pool().await;
        let error = resolve(
            &pool,
            DatabaseBackend::Sqlite,
            METHOD,
            &Encoding::Any,
            &no_errors(),
            blob_ref("bafkreinotstored", Some("application/wasm")),
        )
        .await
        .expect_err("an absent blob should not answer");

        assert!(
            matches!(error, AppError::NotFound(_)),
            "expected a not-found, got {error:?}"
        );
    }

    /// A `*/*` method has to be able to say "not found". `getBlob` declares
    /// `*/*` and answers `BlobNotFound` as JSON, so a structured failure is
    /// passed through rather than refused for not being bytes.
    #[tokio::test]
    async fn a_byte_method_can_still_answer_a_structured_failure() {
        let pool = migrated_memory_pool().await;
        let failure =
            json!({ "error": "ReleaseNotFound", "message": "no release at that version" });

        for encoding in [Encoding::Any, Encoding::Fixed("application/wasm".into())] {
            let answer = resolve(
                &pool,
                DatabaseBackend::Sqlite,
                METHOD,
                &encoding,
                &declaring(&["ReleaseNotFound"]),
                failure.clone(),
            )
            .await
            .expect("a failure should answer");
            match answer {
                ScriptResponse::Failure(value) => assert_eq!(value, failure),
                other => panic!("a declared failure answered as {other:?}"),
            }

            // The lexicon not declaring it is a reason to fix the lexicon, not
            // to refuse the method: it keeps the 200 it has always answered
            // rather than failing the byte checks below.
            let answer = resolve(
                &pool,
                DatabaseBackend::Sqlite,
                METHOD,
                &encoding,
                &no_errors(),
                failure.clone(),
            )
            .await
            .expect("an undeclared failure should still answer");
            match answer {
                ScriptResponse::Json(value) => assert_eq!(value, failure),
                other => panic!("an undeclared failure answered as {other:?}"),
            }
        }
    }

    /// The two tests are both needed: a payload may legitimately carry a field
    /// called `error`, and reading that as a refusal would send JSON where a
    /// caller asked for bytes.
    #[tokio::test]
    async fn a_byte_payload_carrying_an_error_field_is_still_bytes() {
        let pool = migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        let cid = crate::blobs::put(&pool, backend, b"\x00\x01wasm", "application/wasm")
            .await
            .expect("store");

        let mut blob = blob_ref(&cid, Some("application/wasm"));
        blob["error"] = json!("a field of the payload, not a refusal");
        let declared = declaring(&["a field of the payload, not a refusal"]);
        match resolve(&pool, backend, METHOD, &Encoding::Any, &declared, blob)
            .await
            .expect("serve")
        {
            ScriptResponse::Bytes { bytes, .. } => assert_eq!(bytes, b"\x00\x01wasm"),
            other => panic!("a blob ref was read as {other:?}, not bytes"),
        }

        let inline =
            json!({ "$bytes": "aGk=", "mimeType": "text/plain", "error": "not a refusal" });
        match resolve(
            &pool,
            backend,
            METHOD,
            &Encoding::Any,
            &declaring(&["not a refusal"]),
            inline,
        )
        .await
        .expect("serve")
        {
            ScriptResponse::Bytes { bytes, .. } => assert_eq!(bytes, b"hi"),
            other => panic!("$bytes was read as {other:?}, not bytes"),
        }
    }

    /// The status is the whole point: every atproto client reads it before the
    /// body, so a refusal rendered at 200 is indistinguishable from an answer
    /// that happens to carry an `error` field. Asserting the variant alone
    /// would pass with `into_response` still answering 200.
    #[test]
    fn a_refusal_renders_at_400_and_an_answer_at_200() {
        let failure = json!({ "error": "ReleaseNotFound", "message": "no such release" });

        let response = ScriptResponse::Failure(failure.clone()).into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = ScriptResponse::Json(failure).into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// The lexicon is the contract, so the names come from it rather than from
    /// a guess about which strings look like errors.
    #[test]
    fn the_declared_errors_are_read_off_the_lexicon() {
        let parse = |main: Value| {
            ParsedLexicon::parse(
                json!({ "lexicon": 1, "id": "at.example.getThing", "defs": { "main": main } }),
                1,
                None,
                crate::lexicon::ProcedureAction::Create,
                None,
            )
            .expect("the fixture lexicon should parse")
        };

        let declared = declared_errors(&parse(json!({
            "type": "query",
            "errors": [{ "name": "ReleaseNotFound" }, { "name": "ArtifactNotHeld" }],
        })));
        assert_eq!(
            declared,
            declaring(&["ArtifactNotHeld", "ReleaseNotFound"]),
            "both declared names should be read"
        );

        assert!(
            declared_errors(&parse(json!({ "type": "query" }))).is_empty(),
            "a lexicon declaring no errors should yield none"
        );
    }

    /// The event log must not carry the payload. A megabyte of base64 per
    /// request is a cost paid forever for something nobody reads out of a row.
    #[test]
    fn a_byte_response_is_summarised_for_the_event_log() {
        let answer = ScriptResponse::Bytes {
            bytes: vec![7u8; 4096],
            content_type: "application/wasm".into(),
            cid: Some("bafkreiexample".into()),
        };

        assert_eq!(answer.response_size(), 4096);
        assert_eq!(
            answer.log_detail(),
            json!({ "mimeType": "application/wasm", "size": 4096, "cid": "bafkreiexample" })
        );
    }
}
