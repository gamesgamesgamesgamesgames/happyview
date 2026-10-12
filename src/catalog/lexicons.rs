//! Lexicons, layered: a project file, then a manual row, then a network row.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use serde_json::Value;
use tracing::{info, warn};

use super::{Entry, Layer, lexicon_rows};
use crate::lexicon::{LexiconType, ParsedLexicon, ProcedureAction};
use crate::project::{ProjectLexicon, SharedSnapshot, shared_empty};

/// A stored row's parsed copy. A row is manual or network, never both: the
/// table is keyed by id.
#[derive(Debug, Clone)]
struct StoredLexicon {
    parsed: ParsedLexicon,
    layer: Layer,
    authority_did: Option<String>,
}

impl StoredLexicon {
    fn entry(&self) -> Entry<ParsedLexicon> {
        Entry {
            value: self.parsed.clone(),
            layer: self.layer,
            path: None,
        }
    }
}

fn project_entry(lex: &ProjectLexicon) -> Entry<ParsedLexicon> {
    Entry {
        value: lex.parsed.clone(),
        layer: Layer::Project,
        path: Some(lex.path.clone()),
    }
}

/// A network row's parsed copy and the authority its NSID resolved to.
#[derive(Debug, Clone)]
pub struct NetworkLexicon {
    pub parsed: ParsedLexicon,
    pub authority_did: Option<String>,
}

impl From<&StoredLexicon> for NetworkLexicon {
    fn from(s: &StoredLexicon) -> Self {
        Self {
            parsed: s.parsed.clone(),
            authority_did: s.authority_did.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LexiconCatalog {
    stored: Arc<RwLock<HashMap<String, StoredLexicon>>>,
    project: SharedSnapshot,
}

impl Default for LexiconCatalog {
    fn default() -> Self {
        Self::new(shared_empty())
    }
}

impl LexiconCatalog {
    pub fn new(project: SharedSnapshot) -> Self {
        Self {
            stored: Arc::new(RwLock::new(HashMap::new())),
            project,
        }
    }

    // A guard is never held across an `.await`: the lock is a std lock so
    // `shadowed` can answer synchronously, and a std guard held over a
    // suspension point would make every handler future `!Send`. A writer that
    // panicked left the map as it was before its insert, so poisoning is not a
    // reason to stop answering.
    fn stored(&self) -> RwLockReadGuard<'_, HashMap<String, StoredLexicon>> {
        self.stored.read().unwrap_or_else(|e| e.into_inner())
    }

    fn stored_mut(&self) -> RwLockWriteGuard<'_, HashMap<String, StoredLexicon>> {
        self.stored.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Load every stored row, replacing what memory held.
    pub async fn load_from_db(&self, db: &sqlx::AnyPool) -> Result<(), String> {
        let rows = lexicon_rows::load_all(db)
            .await
            .map_err(|e| format!("failed to load lexicons: {e}"))?;

        let mut loaded = HashMap::new();
        for row in rows {
            let id = row.id;
            let json: Value = match serde_json::from_str(&row.lexicon_json) {
                Ok(v) => v,
                Err(e) => {
                    warn!(%id, "failed to parse lexicon_json: {e}");
                    continue;
                }
            };
            let action = match ProcedureAction::from_optional_str(row.action.as_deref()) {
                Ok(a) => a,
                Err(e) => {
                    warn!(%id, "invalid action value: {e}");
                    ProcedureAction::Upsert
                }
            };
            match ParsedLexicon::parse(
                json,
                row.revision,
                row.target_collection,
                action,
                row.token_cost.map(|c| c as u32),
            ) {
                Ok(parsed) => {
                    loaded.insert(
                        id,
                        StoredLexicon {
                            parsed,
                            layer: Layer::from_source(&row.source),
                            authority_did: row.authority_did,
                        },
                    );
                }
                Err(e) => warn!(%id, "failed to parse lexicon: {e}"),
            }
        }

        let count = loaded.len();
        *self.stored_mut() = loaded;
        info!(count, "loaded lexicons into catalog");
        Ok(())
    }

    /// Every id this catalog answers for, with the layer that answers it.
    fn effective(&self) -> BTreeMap<String, Entry<ParsedLexicon>> {
        let mut out: BTreeMap<String, Entry<ParsedLexicon>> = self
            .stored()
            .iter()
            .map(|(id, s)| (id.clone(), s.entry()))
            .collect();
        for (id, lex) in &self.project.load().lexicons {
            out.insert(id.clone(), project_entry(lex));
        }
        out
    }

    fn ids_of_type(&self, lexicon_type: LexiconType) -> Vec<String> {
        self.effective()
            .into_iter()
            .filter(|(_, e)| e.value.lexicon_type == lexicon_type)
            .map(|(id, _)| id)
            .collect()
    }

    pub async fn get_entry(&self, id: &str) -> Option<Entry<ParsedLexicon>> {
        if let Some(lex) = self.project.load().lexicons.get(id) {
            return Some(project_entry(lex));
        }
        self.stored().get(id).map(StoredLexicon::entry)
    }

    pub async fn get(&self, id: &str) -> Option<ParsedLexicon> {
        self.get_entry(id).await.map(|e| e.value)
    }

    /// Sorted, so the Jetstream filter and every listing built from it are
    /// stable between calls.
    pub async fn get_record_collections(&self) -> Vec<String> {
        self.ids_of_type(LexiconType::Record)
    }

    pub async fn get_queries(&self) -> Vec<String> {
        self.ids_of_type(LexiconType::Query)
    }

    pub async fn get_procedures(&self) -> Vec<String> {
        self.ids_of_type(LexiconType::Procedure)
    }

    pub async fn count(&self) -> usize {
        self.effective().len()
    }

    pub async fn get_space_declaration(&self, id: &str) -> Option<ParsedLexicon> {
        self.get(id)
            .await
            .filter(|lex| lex.lexicon_type == LexiconType::Space)
    }

    /// A stored network row, whether or not a project file shadows it: the
    /// row is still the network's to refresh.
    pub async fn get_network(&self, id: &str) -> Option<NetworkLexicon> {
        self.stored()
            .get(id)
            .filter(|s| s.layer == Layer::Network)
            .map(NetworkLexicon::from)
    }

    /// Every stored network row, by id.
    pub async fn network_lexicons(&self) -> Vec<NetworkLexicon> {
        let mut out: Vec<NetworkLexicon> = self
            .stored()
            .values()
            .filter(|s| s.layer == Layer::Network)
            .map(NetworkLexicon::from)
            .collect();
        out.sort_by(|a, b| a.parsed.id.cmp(&b.parsed.id));
        out
    }

    pub async fn count_by_layer(&self) -> BTreeMap<Layer, usize> {
        let mut counts = BTreeMap::new();
        for entry in self.effective().values() {
            *counts.entry(entry.layer).or_insert(0) += 1;
        }
        counts
    }

    /// Stored ids, manual or network, that a project file overrides. The rows
    /// are kept and answer again when the file goes.
    pub fn shadowed(&self) -> Vec<(String, PathBuf)> {
        let project = self.project.load();
        let stored = self.stored();
        project
            .lexicons
            .iter()
            .filter(|(id, _)| stored.contains_key(*id))
            .map(|(id, lex)| (id.clone(), lex.path.clone()))
            .collect()
    }

    /// Puts a lexicon in memory without a row, keeping the layer an existing
    /// entry has.
    pub async fn upsert(&self, parsed: ParsedLexicon) {
        let mut stored = self.stored_mut();
        let (layer, authority_did) = stored
            .get(&parsed.id)
            .map(|s| (s.layer, s.authority_did.clone()))
            .unwrap_or((Layer::Manual, None));
        stored.insert(
            parsed.id.clone(),
            StoredLexicon {
                parsed,
                layer,
                authority_did,
            },
        );
    }

    /// Drops a lexicon from memory without touching its row.
    pub async fn forget(&self, id: &str) -> bool {
        self.stored_mut().remove(id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::project::{ProjectLexicon, ProjectSnapshot, shared_empty};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn record_lexicon_json() -> Value {
        json!({
            "lexicon": 1,
            "id": "games.gamesgamesgamesgames.game",
            "defs": {
                "main": {
                    "type": "record",
                    "key": "tid",
                    "record": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" }
                        }
                    }
                }
            }
        })
    }

    fn query_lexicon_json() -> Value {
        json!({
            "lexicon": 1,
            "id": "games.gamesgamesgamesgames.listGames",
            "defs": {
                "main": {
                    "type": "query",
                    "parameters": {
                        "type": "params",
                        "properties": {
                            "limit": { "type": "integer" }
                        }
                    },
                    "output": {
                        "encoding": "application/json"
                    }
                }
            }
        })
    }

    fn procedure_lexicon_json() -> Value {
        json!({
            "lexicon": 1,
            "id": "games.gamesgamesgamesgames.createGame",
            "defs": {
                "main": {
                    "type": "procedure",
                    "input": {
                        "encoding": "application/json"
                    },
                    "output": {
                        "encoding": "application/json"
                    }
                }
            }
        })
    }

    fn definitions_lexicon_json() -> Value {
        json!({
            "lexicon": 1,
            "id": "games.gamesgamesgamesgames.defs",
            "defs": {
                "genre": {
                    "type": "string",
                    "knownValues": ["action", "rpg"]
                }
            }
        })
    }

    fn space_declaration_lexicon_json() -> Value {
        json!({
            "lexicon": 1,
            "id": "com.example.forum",
            "defs": {
                "main": {
                    "type": "space",
                    "key": "slug",
                    "name": "Forum",
                    "collections": [
                        "com.example.forum.post",
                        "com.example.forum.comment"
                    ]
                }
            }
        })
    }

    fn parse(json: Value, revision: i32) -> ParsedLexicon {
        ParsedLexicon::parse(json, revision, None, ProcedureAction::Upsert, None).unwrap()
    }

    fn snapshot_with(lexicons: &[(Value, &str)]) -> ProjectSnapshot {
        let mut snap = ProjectSnapshot::empty();
        for (json, path) in lexicons {
            let parsed = parse(json.clone(), 0);
            snap.lexicons.insert(
                parsed.id.clone(),
                ProjectLexicon {
                    parsed,
                    path: PathBuf::from(path),
                    backfill: false,
                },
            );
        }
        snap
    }

    #[tokio::test]
    async fn catalog_new_is_empty() {
        let reg = LexiconCatalog::default();
        assert_eq!(reg.count().await, 0);
    }

    #[tokio::test]
    async fn catalog_upsert_and_get() {
        let reg = LexiconCatalog::default();
        reg.upsert(parse(record_lexicon_json(), 1)).await;
        let got = reg.get("games.gamesgamesgamesgames.game").await;
        assert!(got.is_some());
        assert_eq!(got.unwrap().lexicon_type, LexiconType::Record);
    }

    #[tokio::test]
    async fn catalog_upsert_replaces() {
        let reg = LexiconCatalog::default();
        reg.upsert(parse(record_lexicon_json(), 1)).await;
        reg.upsert(parse(record_lexicon_json(), 5)).await;
        assert_eq!(reg.count().await, 1);
        assert_eq!(
            reg.get("games.gamesgamesgamesgames.game")
                .await
                .unwrap()
                .revision,
            5
        );
    }

    #[tokio::test]
    async fn catalog_get_nonexistent() {
        let reg = LexiconCatalog::default();
        assert!(reg.get("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn catalog_type_filtered_collections() {
        let reg = LexiconCatalog::default();
        reg.upsert(parse(record_lexicon_json(), 1)).await;
        reg.upsert(parse(query_lexicon_json(), 1)).await;
        reg.upsert(parse(procedure_lexicon_json(), 1)).await;
        reg.upsert(parse(definitions_lexicon_json(), 1)).await;

        assert_eq!(reg.count().await, 4);
        assert_eq!(
            reg.get_record_collections().await,
            vec!["games.gamesgamesgamesgames.game".to_string()]
        );
        assert_eq!(
            reg.get_queries().await,
            vec!["games.gamesgamesgamesgames.listGames".to_string()]
        );
        assert_eq!(
            reg.get_procedures().await,
            vec!["games.gamesgamesgamesgames.createGame".to_string()]
        );
    }

    #[tokio::test]
    async fn catalog_get_space_declaration() {
        let reg = LexiconCatalog::default();
        reg.upsert(parse(space_declaration_lexicon_json(), 1)).await;

        let decl = reg.get_space_declaration("com.example.forum").await;
        assert!(decl.is_some());
        assert_eq!(
            decl.unwrap().space_collections,
            Some(vec![
                "com.example.forum.post".to_string(),
                "com.example.forum.comment".to_string()
            ])
        );
        assert!(reg.get_space_declaration("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn catalog_get_space_declaration_excludes_non_space_types() {
        let reg = LexiconCatalog::default();
        reg.upsert(parse(record_lexicon_json(), 1)).await;
        assert!(
            reg.get_space_declaration("games.gamesgamesgamesgames.game")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_project_file_answers_ahead_of_a_stored_row_until_it_goes() {
        let project = shared_empty();
        let catalog = LexiconCatalog::new(project.clone());
        catalog.upsert(parse(record_lexicon_json(), 3)).await;

        project.store(Arc::new(snapshot_with(&[(
            record_lexicon_json(),
            "src/lexicons/games/gamesgamesgamesgames/game.json",
        )])));
        let entry = catalog
            .get_entry("games.gamesgamesgamesgames.game")
            .await
            .unwrap();
        assert_eq!(entry.layer, Layer::Project);
        assert_eq!(
            entry.path.as_deref(),
            Some(std::path::Path::new(
                "src/lexicons/games/gamesgamesgamesgames/game.json"
            ))
        );
        assert_eq!(entry.value.revision, 0);

        project.store(Arc::new(ProjectSnapshot::empty()));
        let entry = catalog
            .get_entry("games.gamesgamesgamesgames.game")
            .await
            .unwrap();
        assert_eq!(entry.layer, Layer::Manual);
        assert_eq!(entry.value.revision, 3);
        assert!(entry.path.is_none());
    }

    #[tokio::test]
    async fn record_collections_list_each_id_once_sorted_across_layers() {
        let project = shared_empty();
        let catalog = LexiconCatalog::new(project.clone());
        catalog.upsert(parse(record_lexicon_json(), 1)).await;
        let mut other = record_lexicon_json();
        other["id"] = json!("aaa.example.first");
        project.store(Arc::new(snapshot_with(&[
            (record_lexicon_json(), "src/lexicons/game.json"),
            (other, "src/lexicons/first.json"),
            (query_lexicon_json(), "src/lexicons/list.json"),
        ])));

        assert_eq!(
            catalog.get_record_collections().await,
            vec![
                "aaa.example.first".to_string(),
                "games.gamesgamesgamesgames.game".to_string()
            ]
        );
        assert_eq!(
            catalog.get_queries().await,
            vec!["games.gamesgamesgamesgames.listGames".to_string()]
        );
        assert_eq!(catalog.count().await, 3);
    }

    #[tokio::test]
    async fn shadowed_names_stored_ids_a_project_file_overrides() {
        let project = shared_empty();
        let catalog = LexiconCatalog::new(project.clone());
        catalog.upsert(parse(record_lexicon_json(), 1)).await;
        catalog.upsert(parse(query_lexicon_json(), 1)).await;
        project.store(Arc::new(snapshot_with(&[
            (record_lexicon_json(), "src/lexicons/game.json"),
            (procedure_lexicon_json(), "src/lexicons/create.json"),
        ])));

        assert_eq!(
            catalog.shadowed(),
            vec![(
                "games.gamesgamesgamesgames.game".to_string(),
                PathBuf::from("src/lexicons/game.json")
            )]
        );
    }

    #[tokio::test]
    async fn count_by_layer_counts_each_id_under_the_layer_that_answers_it() {
        let project = shared_empty();
        let catalog = LexiconCatalog::new(project.clone());
        catalog.upsert(parse(record_lexicon_json(), 1)).await;
        catalog.upsert(parse(query_lexicon_json(), 1)).await;
        project.store(Arc::new(snapshot_with(&[
            (record_lexicon_json(), "src/lexicons/game.json"),
            (procedure_lexicon_json(), "src/lexicons/create.json"),
        ])));

        let counts = catalog.count_by_layer().await;
        assert_eq!(counts.get(&Layer::Project), Some(&2));
        assert_eq!(counts.get(&Layer::Manual), Some(&1));
        assert_eq!(counts.get(&Layer::Network), None);
    }

    #[tokio::test]
    async fn a_space_declaration_in_the_project_is_found() {
        let project = shared_empty();
        let catalog = LexiconCatalog::new(project.clone());
        project.store(Arc::new(snapshot_with(&[(
            space_declaration_lexicon_json(),
            "src/lexicons/com/example/forum.json",
        )])));
        assert!(
            catalog
                .get_space_declaration("com.example.forum")
                .await
                .is_some()
        );
    }

    /// The handle `AppState.lexicons` reads is the one `AppState.project`
    /// stores into. Two handles would leave every project lexicon invisible
    /// to every route while each half looked correct on its own.
    #[tokio::test]
    async fn the_app_state_catalog_reads_the_app_state_snapshot() {
        let state =
            crate::test_support::test_state_with_pool(crate::test_support::memory_pool().await);
        state.project.store(Arc::new(snapshot_with(&[(
            record_lexicon_json(),
            "src/lexicons/game.json",
        )])));
        assert!(
            state
                .lexicons
                .get("games.gamesgamesgamesgames.game")
                .await
                .is_some()
        );
    }
}
