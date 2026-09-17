//! An XRPC call issued from inside this instance, on behalf of a script or a
//! plugin: dispatched to a registered local handler when one exists, and to
//! the proxy when none does.

use axum::response::Response;
use serde_json::Value;
use std::collections::HashMap;

use crate::AppState;
use crate::auth::Claims;
use crate::lexicon::LexiconType;
use crate::xrpc;

/// Execute a query XRPC — local handler if known, proxy if not.
pub(crate) async fn execute_local_query(
    state: &AppState,
    method: &str,
    params: &mut HashMap<String, Value>,
    claims: Option<&Claims>,
) -> Result<Response, crate::error::AppError> {
    let lexicon = state.lexicons.get(method).await;

    match lexicon {
        Some(lex) => {
            if lex.lexicon_type != LexiconType::Query {
                return Err(crate::error::AppError::BadRequest(format!(
                    "{method} is not a query endpoint"
                )));
            }
            if let Some(ref param_schema) = lex.parameters {
                xrpc::coerce_params(params, param_schema);
            }
            xrpc::query::handle_query(state, method, params, &lex, claims).await
        }
        None => {
            let query_string = params_to_query_string(params);
            xrpc::proxy_to_authority(state, method, &query_string, None).await
        }
    }
}

/// Build a query string from a params HashMap (used by proxy path).
fn params_to_query_string(params: &HashMap<String, Value>) -> String {
    params
        .iter()
        .map(|(k, v)| {
            let val = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{}={}", urlencoding::encode(k), urlencoding::encode(&val))
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Execute a procedure XRPC — local handler if known, proxy if not.
pub(crate) async fn execute_local_procedure(
    state: &AppState,
    method: &str,
    claims: &Claims,
    input: &Value,
    params: &mut HashMap<String, Value>,
) -> Result<Response, crate::error::AppError> {
    let lexicon = state.lexicons.get(method).await;

    match lexicon {
        Some(lex) => {
            if lex.lexicon_type != LexiconType::Procedure {
                return Err(crate::error::AppError::BadRequest(format!(
                    "{method} is not a procedure endpoint"
                )));
            }
            if let Some(ref param_schema) = lex.parameters {
                xrpc::coerce_params(params, param_schema);
            }
            xrpc::procedure::handle_procedure(state, method, claims, input, params, &lex, None)
                .await
        }
        None => {
            let query_string = params_to_query_string(params);
            xrpc::proxy_to_authority(state, method, &query_string, Some(input)).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::{ParsedLexicon, ProcedureAction};
    use crate::test_support::{memory_pool, test_state_with_pool};
    use http_body_util::BodyExt;
    use serde_json::json;

    async fn seed_script(state: &AppState, trigger: &str, body: &str) {
        crate::db::query(
            r#"
            CREATE TABLE IF NOT EXISTS happyview_scripts (
                id          TEXT PRIMARY KEY,
                body        TEXT NOT NULL,
                description TEXT,
                script_type TEXT NOT NULL DEFAULT 'lua',
                created_at  TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&state.db)
        .await
        .unwrap();
        crate::db::query(
            "INSERT OR REPLACE INTO happyview_scripts (id, body, script_type, created_at, updated_at)
             VALUES (?, ?, 'lua', datetime('now'), datetime('now'))",
        )
        .bind(trigger)
        .bind(body)
        .execute(&state.db)
        .await
        .unwrap();
    }

    fn lexicon(id: &str, lexicon_type: LexiconType) -> ParsedLexicon {
        ParsedLexicon {
            id: id.to_string(),
            lexicon_type,
            record_key: None,
            parameters: None,
            input: None,
            output: None,
            record_schema: None,
            raw: json!({}),
            revision: 1,
            target_collection: None,
            action: ProcedureAction::Create,
            token_cost: None,
            space_type: None,
            space_name: None,
            space_collections: None,
        }
    }

    async fn body_json(response: Response) -> Value {
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[test]
    fn params_to_query_string_empty() {
        let params = HashMap::new();
        assert_eq!(params_to_query_string(&params), "");
    }

    #[test]
    fn params_to_query_string_encodes_values() {
        let mut params = HashMap::new();
        params.insert("handle".into(), Value::String("user.bsky.social".into()));
        let qs = params_to_query_string(&params);
        assert!(qs.contains("handle=user.bsky.social"));
    }

    #[test]
    fn params_to_query_string_url_encodes_special_chars() {
        let mut params = HashMap::new();
        params.insert(
            "uri".into(),
            Value::String("at://did:plc:abc/col/rkey".into()),
        );
        let qs = params_to_query_string(&params);
        assert!(qs.contains("uri=at%3A%2F%2Fdid%3Aplc%3Aabc%2Fcol%2Frkey"));
    }

    #[tokio::test]
    async fn query_local_script_returns_json() {
        let state = test_state_with_pool(memory_pool().await);
        state
            .lexicons
            .upsert(lexicon("test.echo", LexiconType::Query))
            .await;
        seed_script(
            &state,
            "xrpc.query:test.echo",
            r#"function handle() return { greeting = "hello" } end"#,
        )
        .await;

        let mut params = HashMap::new();
        let response = execute_local_query(&state, "test.echo", &mut params, None)
            .await
            .expect("query should run");
        assert_eq!(response.status(), 200);
        assert_eq!(body_json(response).await["greeting"], "hello");
    }

    #[tokio::test]
    async fn query_local_script_receives_params_as_input() {
        let state = test_state_with_pool(memory_pool().await);
        state
            .lexicons
            .upsert(lexicon("test.greet", LexiconType::Query))
            .await;
        seed_script(
            &state,
            "xrpc.query:test.greet",
            r#"function handle(input) return { greeting = "hello " .. input.name } end"#,
        )
        .await;

        let mut params = HashMap::new();
        params.insert("name".into(), Value::String("world".into()));
        let response = execute_local_query(&state, "test.greet", &mut params, None)
            .await
            .expect("query should run");
        assert_eq!(body_json(response).await["greeting"], "hello world");
    }

    #[tokio::test]
    async fn query_local_script_receives_the_caller_in_ctx() {
        let state = test_state_with_pool(memory_pool().await);
        state
            .lexicons
            .upsert(lexicon("test.whoami", LexiconType::Query))
            .await;
        seed_script(
            &state,
            "xrpc.query:test.whoami",
            r#"function handle(input, ctx)
                return { did = ctx.caller_did or "anonymous" }
            end"#,
        )
        .await;

        let claims = Claims::internal("did:plc:testuser".into());
        let mut params = HashMap::new();
        let response = execute_local_query(&state, "test.whoami", &mut params, Some(&claims))
            .await
            .expect("query should run");
        assert_eq!(body_json(response).await["did"], "did:plc:testuser");

        let mut params = HashMap::new();
        let response = execute_local_query(&state, "test.whoami", &mut params, None)
            .await
            .expect("query should run");
        assert_eq!(body_json(response).await["did"], "anonymous");
    }

    #[tokio::test]
    async fn query_rejects_procedure_lexicon() {
        let state = test_state_with_pool(memory_pool().await);
        state
            .lexicons
            .upsert(lexicon("test.create", LexiconType::Procedure))
            .await;

        let mut params = HashMap::new();
        let err = execute_local_query(&state, "test.create", &mut params, None)
            .await
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("not a query endpoint"),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn procedure_rejects_query_lexicon() {
        let state = test_state_with_pool(memory_pool().await);
        state
            .lexicons
            .upsert(lexicon("test.echo", LexiconType::Query))
            .await;

        let claims = Claims::internal("did:plc:test".into());
        let mut params = HashMap::new();
        let err = execute_local_procedure(&state, "test.echo", &claims, &json!({}), &mut params)
            .await
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("not a procedure endpoint"),
            "got: {err:?}"
        );
    }
}
