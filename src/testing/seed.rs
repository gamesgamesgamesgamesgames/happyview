//! Seeding the manual catalog layer, for host tests that exercise a trigger
//! without building a project.

use serde_json::Value;

use crate::AppState;
use crate::catalog::{LexiconWrite, ScriptWrite};
use crate::lexicon::ProcedureAction;

pub async fn lexicon(state: &AppState, doc: Value) {
    state
        .lexicons
        .put_manual(
            &state.db,
            state.db_backend,
            LexiconWrite {
                lexicon_json: doc,
                backfill: false,
                target_collection: None,
                action: ProcedureAction::Upsert,
                token_cost: None,
                authority_did: None,
                indexed: true,
            },
        )
        .await
        .expect("seed a lexicon");
}

pub async fn script(state: &AppState, trigger: &str, language: &str, body: &str) {
    state
        .scripts
        .put_manual(
            &state.db,
            state.db_backend,
            ScriptWrite {
                id: trigger.to_string(),
                script_type: language.to_string(),
                body: body.to_string(),
                description: None,
            },
        )
        .await
        .expect("seed a script");
}
