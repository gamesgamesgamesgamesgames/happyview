//! Library namespaces answered from data. A mock is data rather than a
//! function in the test because a function would mean re-entering the test's
//! interpreter while it is suspended inside the handler's library call.

use std::collections::{BTreeMap, VecDeque};

use serde_json::Value;

use crate::AppState;
use crate::plugin::{PluginType, TestMock};

use super::harness::{TestHarness, TestHostError};

#[derive(Debug, Default)]
pub struct Mocks {
    by_namespace: BTreeMap<String, BTreeMap<String, Answers>>,
}

#[derive(Debug)]
enum Answers {
    Always(Value),
    InOrder {
        queue: VecDeque<Value>,
        total: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockError {
    pub code: String,
    pub message: String,
}

impl Mocks {
    pub fn set(&mut self, namespace: &str, responses: &Value) -> Result<(), String> {
        let Value::Object(methods) = responses else {
            return Err(format!(
                "a mock of {namespace} is an object of method names, each answered by a response or a list of them"
            ));
        };
        let table = methods
            .iter()
            .map(|(method, answer)| {
                let answers = match answer {
                    Value::Array(list) => Answers::InOrder {
                        queue: list.iter().cloned().collect(),
                        total: list.len(),
                    },
                    single => Answers::Always(single.clone()),
                };
                (method.clone(), answers)
            })
            .collect();
        self.by_namespace.insert(namespace.to_string(), table);
        Ok(())
    }

    /// `None` when the namespace is not mocked, so the call runs for real.
    pub fn answer(&mut self, namespace: &str, method: &str) -> Option<Result<Value, MockError>> {
        let table = self.by_namespace.get_mut(namespace)?;
        let mocked: Vec<String> = table.keys().cloned().collect();
        let Some(answers) = table.get_mut(method) else {
            return Some(Err(MockError {
                code: "NOT_MOCKED".into(),
                message: format!(
                    "{namespace} is mocked, and its mock has no response for '{method}'; mocked: {}",
                    mocked.join(", ")
                ),
            }));
        };
        let value = match answers {
            Answers::Always(value) => value.clone(),
            Answers::InOrder { queue, total } => match queue.pop_front() {
                Some(value) => value,
                None => {
                    return Some(Err(MockError {
                        code: "MOCK_EXHAUSTED".into(),
                        message: format!(
                            "the mock for {namespace}.{method} has answered all {total} of its responses"
                        ),
                    }));
                }
            },
        };
        Some(match value.get("$error") {
            Some(error) => Err(MockError {
                code: error["code"].as_str().unwrap_or("MOCK_ERROR").to_string(),
                message: error["message"].as_str().unwrap_or("").to_string(),
            }),
            None => Ok(value),
        })
    }
}

/// `host_test_mock`: refuse a namespace nothing installed serves, because a
/// mock of a typo would leave the real call running.
pub async fn mock(
    state: &AppState,
    harness: &TestHarness,
    spec: TestMock,
) -> Result<(), TestHostError> {
    let installed = state
        .plugin_registry
        .list_by_type(PluginType::Library)
        .await
        .iter()
        .any(|plugin| plugin.namespace() == Some(spec.namespace.as_str()));
    if !installed {
        return Err(TestHostError::BadInput(format!(
            "no installed library serves '{}'; add it to [libraries] in happyview.toml",
            spec.namespace
        )));
    }
    harness
        .mocks()
        .lock()
        .unwrap()
        .set(&spec.namespace, &spec.responses)
        .map_err(TestHostError::BadInput)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_single_response_answers_every_call() {
        let mut mocks = Mocks::default();
        mocks.set("http", &json!({"get": {"status": 200}})).unwrap();
        for _ in 0..3 {
            assert_eq!(
                mocks.answer("http", "get").unwrap().unwrap(),
                json!({"status": 200})
            );
        }
    }

    #[test]
    fn a_list_answers_in_order_and_then_refuses() {
        let mut mocks = Mocks::default();
        mocks.set("http", &json!({"get": [1, 2]})).unwrap();
        assert_eq!(mocks.answer("http", "get").unwrap().unwrap(), json!(1));
        assert_eq!(mocks.answer("http", "get").unwrap().unwrap(), json!(2));
        let err = mocks.answer("http", "get").unwrap().unwrap_err();
        assert_eq!(err.code, "MOCK_EXHAUSTED");
        assert!(err.message.contains("2"), "{}", err.message);
    }

    #[test]
    fn an_error_response_fails_the_call_with_its_code() {
        let mut mocks = Mocks::default();
        mocks
            .set(
                "http",
                &json!({"get": {"$error": {"code": "TIMEOUT", "message": "slow"}}}),
            )
            .unwrap();
        let err = mocks.answer("http", "get").unwrap().unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.message.as_str()),
            ("TIMEOUT", "slow")
        );
    }

    #[test]
    fn an_unmocked_method_of_a_mocked_namespace_names_what_is_mocked() {
        let mut mocks = Mocks::default();
        mocks.set("http", &json!({"get": 1})).unwrap();
        let err = mocks.answer("http", "post").unwrap().unwrap_err();
        assert_eq!(err.code, "NOT_MOCKED");
        assert!(
            err.message.contains("post") && err.message.contains("get"),
            "{}",
            err.message
        );
        assert!(
            mocks.answer("db", "query").is_none(),
            "an unmocked namespace runs for real"
        );
    }

    #[test]
    fn a_second_mock_replaces_the_first_and_its_cursor() {
        let mut mocks = Mocks::default();
        mocks.set("http", &json!({"get": [1]})).unwrap();
        mocks.answer("http", "get");
        mocks.set("http", &json!({"get": [5]})).unwrap();
        assert_eq!(mocks.answer("http", "get").unwrap().unwrap(), json!(5));
    }

    #[test]
    fn responses_must_be_an_object_of_methods() {
        assert!(Mocks::default().set("http", &json!([1])).is_err());
    }
}
