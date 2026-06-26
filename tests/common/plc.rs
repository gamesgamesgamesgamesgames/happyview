use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

pub type PlcStore = Arc<RwLock<HashMap<String, Value>>>;

struct PlcGetResponder {
    store: PlcStore,
}

impl Respond for PlcGetResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let path = request.url.path();
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        if segments.is_empty() {
            return ResponseTemplate::new(404);
        }

        let did = segments[0];
        let store = self.store.clone();
        let did_owned = did.to_string();

        if segments.len() >= 3 && segments[1] == "log" && segments[2] == "last" {
            let store = futures::executor::block_on(store.read());
            return match store.get(&did_owned) {
                Some(doc) => ResponseTemplate::new(200).set_body_json(doc.clone()),
                None => ResponseTemplate::new(404),
            };
        }

        let store = futures::executor::block_on(store.read());
        match store.get(&did_owned) {
            Some(doc) => ResponseTemplate::new(200).set_body_json(doc.clone()),
            None => ResponseTemplate::new(404),
        }
    }
}

struct PlcPostResponder {
    store: PlcStore,
}

impl Respond for PlcPostResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let path = request.url.path();
        let did = path.trim_start_matches('/').to_string();

        if let Ok(body) = serde_json::from_slice::<Value>(&request.body) {
            let store = self.store.clone();
            futures::executor::block_on(async {
                store.write().await.insert(did, body);
            });
        }

        ResponseTemplate::new(200)
    }
}

pub async fn setup_mock_plc(server: &MockServer) -> PlcStore {
    let store: PlcStore = Arc::new(RwLock::new(HashMap::new()));

    Mock::given(method("GET"))
        .respond_with(PlcGetResponder {
            store: store.clone(),
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .respond_with(PlcPostResponder {
            store: store.clone(),
        })
        .mount(server)
        .await;

    store
}

pub fn test_did_document(did: &str, public_key_bytes: &[u8]) -> Value {
    let mut multikey = vec![0x80, 0x24];
    multikey.extend_from_slice(public_key_bytes);
    let multibase_key = multibase::encode(multibase::Base::Base58Btc, &multikey);

    json!({
        "@context": ["https://www.w3.org/ns/did/v1", "https://w3id.org/security/multikey/v1"],
        "id": did,
        "verificationMethod": [{
            "id": format!("{did}#atproto"),
            "type": "Multikey",
            "controller": did,
            "publicKeyMultibase": multibase_key
        }],
        "service": []
    })
}
