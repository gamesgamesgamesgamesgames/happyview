//! The PDS a test run talks to instead of the network. It answers the four
//! repo writes with real CIDs, so what the index mirrors is byte-for-byte what
//! a PDS would have said, and it remembers each write for the test to read.
//! It does not enforce `swapRecord`: it holds no repo to compare against.

use std::sync::Mutex;

use serde_json::{Value, json};

use crate::plugin::TestPdsWrite;

#[derive(Debug, Default)]
pub struct FakePds {
    writes: Mutex<Vec<TestPdsWrite>>,
}

/// An XRPC error as a PDS words one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakePdsError {
    pub status: u16,
    pub error: &'static str,
    pub message: String,
}

impl FakePdsError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            error: "InvalidRequest",
            message: message.into(),
        }
    }

    pub fn not_served(method: &str) -> Self {
        Self {
            status: 501,
            error: "MethodNotImplemented",
            message: format!(
                "the test PDS answers createRecord, putRecord, deleteRecord and uploadBlob; {method} is not one of them"
            ),
        }
    }

    pub fn body(&self) -> Value {
        json!({"error": self.error, "message": self.message})
    }
}

impl std::fmt::Display for FakePdsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.error, self.status, self.message)
    }
}

fn text<'a>(body: &'a Value, field: &str) -> Result<&'a str, FakePdsError> {
    body.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| FakePdsError::invalid(format!("{field} is required")))
}

impl FakePds {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer `method` for the account `did` is signed in as.
    pub fn post(&self, did: &str, method: &str, body: &Value) -> Result<Value, FakePdsError> {
        let action = match method {
            "com.atproto.repo.createRecord" => "create",
            "com.atproto.repo.putRecord" => "put",
            "com.atproto.repo.deleteRecord" => "delete",
            _ => return Err(FakePdsError::not_served(method)),
        };
        let repo = text(body, "repo")?;
        if repo != did {
            return Err(FakePdsError::invalid(format!(
                "the test PDS is signed in as {did}, so it cannot write to {repo}"
            )));
        }
        let collection = text(body, "collection")?.to_string();
        let rkey = match (action, body.get("rkey").and_then(Value::as_str)) {
            (_, Some(rkey)) => rkey.to_string(),
            ("create", None) => crate::tid::generate_tid(),
            (_, None) => return Err(FakePdsError::invalid("rkey is required")),
        };
        let uri = format!("at://{repo}/{collection}/{rkey}");

        if action == "delete" {
            self.writes.lock().unwrap().push(TestPdsWrite {
                action: action.into(),
                repo: repo.into(),
                collection,
                rkey,
                record: None,
                cid: None,
            });
            return Ok(json!({}));
        }

        let mut record = body
            .get("record")
            .filter(|r| r.is_object())
            .cloned()
            .ok_or_else(|| FakePdsError::invalid("record must be an object"))?;
        // A PDS fills in `$type`, and the CID is over what it stores.
        if let Some(object) = record.as_object_mut() {
            object
                .entry("$type")
                .or_insert_with(|| Value::String(collection.clone()));
        }
        let cid = crate::cid_verify::compute_record_cid(&record)
            .ok_or_else(|| FakePdsError::invalid("the record cannot be encoded as DAG-CBOR"))?
            .to_string();
        self.writes.lock().unwrap().push(TestPdsWrite {
            action: action.into(),
            repo: repo.into(),
            collection,
            rkey,
            record: Some(record),
            cid: Some(cid.clone()),
        });
        Ok(json!({"uri": uri, "cid": cid, "validationStatus": "unknown"}))
    }

    /// Blobs are not record writes, so they are not in [`writes`](Self::writes).
    pub fn upload_blob(&self, mime: &str, bytes: &[u8]) -> Result<Value, FakePdsError> {
        let cid = crate::cid_verify::raw_cid(bytes)
            .ok_or_else(|| FakePdsError::invalid("the blob could not be hashed"))?
            .to_string();
        Ok(json!({"blob": {
            "$type": "blob",
            "ref": {"$link": cid},
            "mimeType": mime,
            "size": bytes.len(),
        }}))
    }

    /// Every write accepted since the last reported test, in order.
    pub fn writes(&self) -> Vec<TestPdsWrite> {
        self.writes.lock().unwrap().clone()
    }

    pub(crate) fn clear(&self) {
        self.writes.lock().unwrap().clear();
    }
}

/// A fake's answer as the HTTP response the real client would have read.
pub fn to_response(result: Result<Value, FakePdsError>) -> reqwest::Response {
    let (status, body) = match result {
        Ok(body) => (200, body),
        Err(e) => (e.status, e.body()),
    };
    let response = axum::http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&body).unwrap_or_default())
        .expect("a status and a header always build");
    reqwest::Response::from(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn create_answers_a_real_cid_and_records_the_write() {
        let pds = FakePds::new();
        let record = json!({"text": "hi"});
        let out = pds
            .post(
                "did:plc:alice",
                "com.atproto.repo.createRecord",
                &json!({"repo": "did:plc:alice", "collection": "app.test.rec", "rkey": "1", "record": record}),
            )
            .unwrap();
        let stored = json!({"text": "hi", "$type": "app.test.rec"});
        let cid = crate::cid_verify::compute_record_cid(&stored)
            .unwrap()
            .to_string();
        assert_eq!(out["uri"], "at://did:plc:alice/app.test.rec/1");
        assert_eq!(out["cid"], cid);
        let writes = pds.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].action, "create");
        assert_eq!(writes[0].record, Some(stored));
        assert_eq!(writes[0].cid.as_deref(), Some(cid.as_str()));
    }

    #[test]
    fn create_mints_an_rkey_when_none_is_given() {
        let pds = FakePds::new();
        let out = pds
            .post(
                "did:plc:a",
                "com.atproto.repo.createRecord",
                &json!({"repo": "did:plc:a", "collection": "c.d.e", "record": {}}),
            )
            .unwrap();
        let rkey = out["uri"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_string();
        assert_eq!(rkey.len(), 13, "a TID: {rkey}");
    }

    #[test]
    fn a_write_to_another_repo_is_refused() {
        let err = FakePds::new()
            .post(
                "did:plc:a",
                "com.atproto.repo.createRecord",
                &json!({"repo": "did:plc:b", "collection": "c.d.e", "record": {}}),
            )
            .unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("did:plc:b"), "{}", err.message);
    }

    #[test]
    fn put_and_delete_are_recorded_and_delete_carries_no_record() {
        let pds = FakePds::new();
        pds.post(
            "did:plc:a",
            "com.atproto.repo.putRecord",
            &json!({"repo": "did:plc:a", "collection": "c.d.e", "rkey": "1", "record": {"x": 1}}),
        )
        .unwrap();
        pds.post(
            "did:plc:a",
            "com.atproto.repo.deleteRecord",
            &json!({"repo": "did:plc:a", "collection": "c.d.e", "rkey": "1"}),
        )
        .unwrap();
        let writes = pds.writes();
        assert_eq!(writes[0].action, "put");
        assert_eq!(writes[1].action, "delete");
        assert!(writes[1].record.is_none() && writes[1].cid.is_none());
    }

    #[test]
    fn an_unserved_method_is_a_501_naming_it() {
        let err = FakePds::new()
            .post("did:plc:a", "com.atproto.repo.applyWrites", &json!({}))
            .unwrap_err();
        assert_eq!(err.status, 501);
        assert!(err.message.contains("applyWrites"));
    }

    #[test]
    fn a_blob_is_addressed_by_its_raw_cid() {
        let blob = FakePds::new().upload_blob("image/png", b"png").unwrap();
        let cid = crate::cid_verify::raw_cid(b"png").unwrap().to_string();
        assert_eq!(blob["blob"]["ref"]["$link"], cid);
        assert_eq!(blob["blob"]["mimeType"], "image/png");
        assert_eq!(blob["blob"]["size"], 3);
    }
}
