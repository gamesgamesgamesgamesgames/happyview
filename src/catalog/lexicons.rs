//! Lexicons, layered: a project file, then a manual row, then a network row.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use serde_json::Value;
use sqlx::AnyPool;
use tracing::{info, warn};

use super::{CatalogError, Entry, Layer, lexicon_rows};
use crate::db::DatabaseBackend;
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

/// What an operator or the network asks to store.
#[derive(Debug, Clone)]
pub struct LexiconWrite {
    pub lexicon_json: Value,
    pub backfill: bool,
    pub target_collection: Option<String>,
    pub action: ProcedureAction,
    pub token_cost: Option<i32>,
    /// The DID the NSID resolved to. Required by `put_network`, ignored by
    /// `put_manual`.
    pub authority_did: Option<String>,
}

impl LexiconWrite {
    fn id(&self) -> Result<String, CatalogError> {
        self.lexicon_json
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                CatalogError::Invalid("lexicon JSON must have a string 'id' field".into())
            })
    }

    fn parse(&self, revision: i32) -> Result<ParsedLexicon, CatalogError> {
        ParsedLexicon::parse(
            self.lexicon_json.clone(),
            revision,
            self.target_collection.clone(),
            self.action.clone(),
            self.token_cost.map(|c| c as u32),
        )
        .map_err(|e| CatalogError::Invalid(format!("failed to parse lexicon: {e}")))
    }
}

/// One lexicon as the admin API lists it: the layer answering for the id,
/// and the stored row's columns when there is one.
#[derive(Debug, Clone)]
pub struct LexiconListing {
    pub id: String,
    pub layer: Layer,
    pub path: Option<PathBuf>,
    /// A stored row sits under this project file. It is kept, untouched, and
    /// answers again when the file goes.
    pub shadows_stored: bool,
    pub lexicon_json: Value,
    pub revision: i32,
    pub backfill: bool,
    pub action: Option<String>,
    pub target_collection: Option<String>,
    pub authority_did: Option<String>,
    pub last_fetched_at: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub token_cost: Option<i32>,
}

impl LexiconListing {
    fn from_row(row: lexicon_rows::FullRow) -> Self {
        Self {
            layer: Layer::from_source(&row.source),
            path: None,
            shadows_stored: false,
            // A row that no longer parses is listed anyway, so an operator can
            // find and delete it.
            lexicon_json: serde_json::from_str(&row.lexicon_json).unwrap_or_default(),
            id: row.id,
            revision: row.revision,
            backfill: row.backfill,
            action: row.action,
            target_collection: row.target_collection,
            authority_did: row.authority_did,
            last_fetched_at: row.last_fetched_at,
            created_at: Some(row.created_at),
            updated_at: Some(row.updated_at),
            token_cost: row.token_cost,
        }
    }

    fn from_project(lex: &crate::project::ProjectLexicon, shadows_stored: bool) -> Self {
        Self {
            id: lex.parsed.id.clone(),
            layer: Layer::Project,
            path: Some(lex.path.clone()),
            shadows_stored,
            lexicon_json: lex.parsed.raw.clone(),
            revision: lex.parsed.revision,
            backfill: lex.backfill,
            action: lex.parsed.action.to_optional_str().map(str::to_string),
            target_collection: lex.parsed.target_collection.clone(),
            authority_did: None,
            last_fetched_at: None,
            created_at: None,
            updated_at: None,
            token_cost: lex.parsed.token_cost.map(|c| c as i32),
        }
    }
}

/// Why a network-driven write failed. The database error stays whole
/// because the Jetstream handler classifies it: an outage is retried, a
/// refusal is not.
#[derive(Debug, thiserror::Error)]
pub enum NetworkWriteError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl From<NetworkWriteError> for CatalogError {
    fn from(e: NetworkWriteError) -> Self {
        match e {
            NetworkWriteError::Invalid(message) => CatalogError::Invalid(message),
            NetworkWriteError::Db(e) => e.into(),
        }
    }
}

impl From<NetworkWriteError> for crate::error::AppError {
    fn from(e: NetworkWriteError) -> Self {
        CatalogError::from(e).into()
    }
}

#[derive(Debug, Clone)]
pub struct LexiconCatalog {
    stored: Arc<RwLock<HashMap<String, StoredLexicon>>>,
    /// Held across each row write and the memory update that follows it.
    /// Without it two writers to one id can commit in one order and update
    /// memory in the other, leaving memory on a version the table no longer
    /// holds. Writes are rare, so serialising them costs nothing.
    write_gate: Arc<tokio::sync::Mutex<()>>,
    project: SharedSnapshot,
}

impl LexiconCatalog {
    /// A catalog with an empty project layer, for a process that runs no
    /// project, and for tests.
    pub fn detached() -> Self {
        Self::new(shared_empty())
    }

    pub fn new(project: SharedSnapshot) -> Self {
        Self {
            stored: Arc::new(RwLock::new(HashMap::new())),
            write_gate: Arc::new(tokio::sync::Mutex::new(())),
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

    /// Refuses an id a project file owns. Writes an operator asks for check
    /// this; writes the network drives do not.
    pub fn check_writable(&self, id: &str) -> Result<(), CatalogError> {
        match self.project.load().lexicons.get(id) {
            Some(lex) => Err(CatalogError::ProjectOwned {
                path: lex.path.clone(),
            }),
            None => Ok(()),
        }
    }

    pub async fn put_manual(
        &self,
        db: &AnyPool,
        backend: DatabaseBackend,
        lexicon: LexiconWrite,
    ) -> Result<i32, CatalogError> {
        let id = lexicon.id()?;
        self.check_writable(&id)?;
        let mut parsed = lexicon.parse(1)?;
        let _gate = self.write_gate.lock().await;
        let revision = lexicon_rows::upsert_manual(db, backend, &id, &lexicon).await?;
        parsed.revision = revision;
        self.stored_mut().insert(
            id,
            StoredLexicon {
                parsed,
                layer: Layer::Manual,
                authority_did: None,
            },
        );
        Ok(revision)
    }

    pub async fn put_network(
        &self,
        db: &AnyPool,
        backend: DatabaseBackend,
        lexicon: LexiconWrite,
    ) -> Result<i32, CatalogError> {
        let id = lexicon.id()?;
        self.check_writable(&id)?;
        let authority_did = lexicon.authority_did.clone().ok_or_else(|| {
            CatalogError::Invalid("a network lexicon needs the DID its NSID resolved to".into())
        })?;
        let mut parsed = lexicon.parse(1)?;
        let _gate = self.write_gate.lock().await;
        let revision =
            lexicon_rows::upsert_network(db, backend, &id, &lexicon, &authority_did).await?;
        parsed.revision = revision;
        self.stored_mut().insert(
            id,
            StoredLexicon {
                parsed,
                layer: Layer::Network,
                authority_did: Some(authority_did),
            },
        );
        Ok(revision)
    }

    /// Replaces a network row's document, keeping its other settings. `None`
    /// when no network row has this id. A row memory could not load is read
    /// from the table, so refreshing it repairs it.
    pub async fn refresh_network(
        &self,
        db: &AnyPool,
        backend: DatabaseBackend,
        id: &str,
        lexicon_json: Value,
    ) -> Result<Option<i32>, NetworkWriteError> {
        let _gate = self.write_gate.lock().await;
        let current = match self.get_network(id).await {
            Some(current) => current,
            None => match self.unloaded_network_row(db, backend, id).await? {
                Some(current) => current,
                None => return Ok(None),
            },
        };
        let mut parsed = ParsedLexicon::parse(
            lexicon_json.clone(),
            1,
            current.parsed.target_collection.clone(),
            current.parsed.action.clone(),
            current.parsed.token_cost,
        )
        .map_err(|e| NetworkWriteError::Invalid(format!("failed to parse lexicon: {e}")))?;
        let Some(revision) = crate::db::retry_on_busy(|| {
            lexicon_rows::refresh_network(db, backend, id, &lexicon_json)
        })
        .await?
        else {
            return Ok(None);
        };
        parsed.revision = revision;
        self.stored_mut().insert(
            id.to_string(),
            StoredLexicon {
                parsed,
                layer: Layer::Network,
                authority_did: current.authority_did,
            },
        );
        Ok(Some(revision))
    }

    /// The settings of a network row that is in the table but not in memory,
    /// because its document no longer parsed when memory loaded.
    async fn unloaded_network_row(
        &self,
        db: &AnyPool,
        backend: DatabaseBackend,
        id: &str,
    ) -> Result<Option<NetworkLexicon>, NetworkWriteError> {
        let Some(row) = lexicon_rows::get_full(db, backend, id).await? else {
            return Ok(None);
        };
        if row.source != "network" {
            return Ok(None);
        }
        // A placeholder document: the caller replaces it, and only the row's
        // settings are read from it.
        let action = ProcedureAction::from_optional_str(row.action.as_deref())
            .unwrap_or(ProcedureAction::Upsert);
        let placeholder = ParsedLexicon::parse(
            serde_json::json!({"lexicon": 1, "id": id, "defs": {}}),
            row.revision,
            row.target_collection,
            action,
            row.token_cost.map(|c| c as u32),
        )
        .map_err(|e| NetworkWriteError::Invalid(format!("failed to parse lexicon: {e}")))?;
        Ok(Some(NetworkLexicon {
            parsed: placeholder,
            authority_did: row.authority_did,
        }))
    }

    pub async fn remove(
        &self,
        db: &AnyPool,
        backend: DatabaseBackend,
        id: &str,
    ) -> Result<bool, CatalogError> {
        self.check_writable(id)?;
        let _gate = self.write_gate.lock().await;
        let removed = lexicon_rows::delete(db, backend, id).await? > 0;
        if removed {
            self.stored_mut().remove(id);
        }
        Ok(removed)
    }

    /// Drops a network row and leaves a manual one alone. On an error memory
    /// is left alone too, so it and the table still agree.
    pub async fn remove_network(
        &self,
        db: &AnyPool,
        backend: DatabaseBackend,
        id: &str,
    ) -> Result<bool, NetworkWriteError> {
        let _gate = self.write_gate.lock().await;
        let removed =
            crate::db::retry_on_busy(|| lexicon_rows::delete_network(db, backend, id)).await? > 0;
        if removed {
            self.stored_mut().remove(id);
        }
        Ok(removed)
    }

    /// Every stored row plus every project lexicon, by id. Rows come from the
    /// table rather than memory, so a row too broken to load is still listed.
    pub async fn list(
        &self,
        db: &AnyPool,
        backend: DatabaseBackend,
    ) -> Result<Vec<LexiconListing>, CatalogError> {
        let project = self.project.load_full();
        let mut out: BTreeMap<String, LexiconListing> = BTreeMap::new();
        for row in lexicon_rows::list_full(db, backend).await? {
            out.insert(row.id.clone(), LexiconListing::from_row(row));
        }
        for (id, lex) in &project.lexicons {
            let shadows_stored = out.contains_key(id);
            out.insert(
                id.clone(),
                LexiconListing::from_project(lex, shadows_stored),
            );
        }
        Ok(out.into_values().collect())
    }

    /// The stored network rows, shadowed or not, since each is the network's
    /// to refresh or stop watching.
    pub async fn list_network(
        &self,
        db: &AnyPool,
        backend: DatabaseBackend,
    ) -> Result<Vec<LexiconListing>, CatalogError> {
        Ok(lexicon_rows::list_full(db, backend)
            .await?
            .into_iter()
            .filter(|row| row.source == "network")
            .map(LexiconListing::from_row)
            .collect())
    }

    pub async fn get_listing(
        &self,
        db: &AnyPool,
        backend: DatabaseBackend,
        id: &str,
    ) -> Result<Option<LexiconListing>, CatalogError> {
        let project = self.project.load_full();
        let row = lexicon_rows::get_full(db, backend, id).await?;
        Ok(match project.lexicons.get(id) {
            Some(lex) => Some(LexiconListing::from_project(lex, row.is_some())),
            None => row.map(LexiconListing::from_row),
        })
    }

    /// Puts a lexicon in memory as a manual entry with no row, for tests that
    /// exercise readers rather than storage.
    #[cfg(test)]
    pub(crate) async fn upsert(&self, parsed: ParsedLexicon) {
        self.stored_mut().insert(
            parsed.id.clone(),
            StoredLexicon {
                parsed,
                layer: Layer::Manual,
                authority_did: None,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::project::{ProjectLexicon, ProjectSnapshot, shared_empty};
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::catalog::CatalogError;

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
        let reg = LexiconCatalog::detached();
        assert_eq!(reg.count().await, 0);
    }

    #[tokio::test]
    async fn catalog_upsert_and_get() {
        let reg = LexiconCatalog::detached();
        reg.upsert(parse(record_lexicon_json(), 1)).await;
        let got = reg.get("games.gamesgamesgamesgames.game").await;
        assert!(got.is_some());
        assert_eq!(got.unwrap().lexicon_type, LexiconType::Record);
    }

    #[tokio::test]
    async fn catalog_upsert_replaces() {
        let reg = LexiconCatalog::detached();
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
        let reg = LexiconCatalog::detached();
        assert!(reg.get("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn catalog_type_filtered_collections() {
        let reg = LexiconCatalog::detached();
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
        let reg = LexiconCatalog::detached();
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
        let reg = LexiconCatalog::detached();
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

    use crate::db::DatabaseBackend;
    use sqlx::AnyPool;

    /// SQLite always; Postgres when `TEST_DATABASE_URL` names one. Ids are
    /// unique per call because the Postgres test database is shared.
    async fn backends() -> Vec<(AnyPool, DatabaseBackend)> {
        crate::test_support::test_pools().await
    }

    fn unique(name: &str) -> String {
        let tag = uuid::Uuid::new_v4().simple().to_string();
        format!("com.example.c{}.{name}", &tag[..10])
    }

    fn record_json(id: &str) -> Value {
        json!({ "lexicon": 1, "id": id, "defs": { "main": { "type": "record", "key": "tid" } } })
    }

    fn manual(json: Value) -> LexiconWrite {
        LexiconWrite {
            lexicon_json: json,
            backfill: false,
            target_collection: None,
            action: ProcedureAction::Upsert,
            token_cost: None,
            authority_did: None,
        }
    }

    fn network(json: Value, authority: &str) -> LexiconWrite {
        LexiconWrite {
            authority_did: Some(authority.to_string()),
            ..manual(json)
        }
    }

    fn owning(id: &str, path: &str) -> ProjectSnapshot {
        snapshot_with(&[(record_json(id), path)])
    }

    #[tokio::test]
    async fn put_manual_writes_the_row_and_memory_together() {
        for (db, backend) in backends().await {
            let catalog = LexiconCatalog::detached();
            let id = unique("thing");

            assert_eq!(
                catalog
                    .put_manual(&db, backend, manual(record_json(&id)))
                    .await
                    .unwrap(),
                1
            );
            assert_eq!(
                catalog
                    .put_manual(&db, backend, manual(record_json(&id)))
                    .await
                    .unwrap(),
                2
            );
            assert_eq!(catalog.get(&id).await.unwrap().revision, 2);

            let fresh = LexiconCatalog::detached();
            fresh.load_from_db(&db).await.unwrap();
            let entry = fresh.get_entry(&id).await.unwrap();
            assert_eq!((entry.layer, entry.value.revision), (Layer::Manual, 2));

            assert!(catalog.remove(&db, backend, &id).await.unwrap());
            assert!(catalog.get(&id).await.is_none());
            assert!(!catalog.remove(&db, backend, &id).await.unwrap());
        }
    }

    #[tokio::test]
    async fn put_manual_refuses_a_project_owned_id_and_writes_nothing() {
        for (db, backend) in backends().await {
            let project = shared_empty();
            let catalog = LexiconCatalog::new(project.clone());
            let id = unique("owned");
            project.store(Arc::new(owning(&id, "src/lexicons/owned.json")));

            let err = catalog
                .put_manual(&db, backend, manual(record_json(&id)))
                .await
                .unwrap_err();
            assert!(
                matches!(&err, CatalogError::ProjectOwned { path } if path == std::path::Path::new("src/lexicons/owned.json")),
                "{err:?}"
            );
            assert!(
                lexicon_rows::get_full(&db, backend, &id)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn remove_refuses_a_shadowed_id_and_the_row_answers_again_when_the_file_goes() {
        for (db, backend) in backends().await {
            let project = shared_empty();
            let catalog = LexiconCatalog::new(project.clone());
            let id = unique("shadowed");
            catalog
                .put_manual(&db, backend, manual(record_json(&id)))
                .await
                .unwrap();
            project.store(Arc::new(owning(&id, "src/lexicons/shadowed.json")));

            assert!(matches!(
                catalog.remove(&db, backend, &id).await,
                Err(CatalogError::ProjectOwned { .. })
            ));
            let listing = catalog
                .get_listing(&db, backend, &id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(listing.layer, Layer::Project);
            assert!(listing.shadows_stored);
            assert_eq!(catalog.shadowed().len(), 1);

            project.store(Arc::new(ProjectSnapshot::empty()));
            let listing = catalog
                .get_listing(&db, backend, &id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!((listing.layer, listing.revision), (Layer::Manual, 1));
            assert!(catalog.remove(&db, backend, &id).await.unwrap());
        }
    }

    #[tokio::test]
    async fn put_network_needs_an_authority_and_records_it() {
        for (db, backend) in backends().await {
            let catalog = LexiconCatalog::detached();
            let id = unique("net");

            assert!(matches!(
                catalog
                    .put_network(&db, backend, manual(record_json(&id)))
                    .await,
                Err(CatalogError::Invalid(_))
            ));
            catalog
                .put_network(&db, backend, network(record_json(&id), "did:plc:authority"))
                .await
                .unwrap();
            let net = catalog.get_network(&id).await.unwrap();
            assert_eq!(net.authority_did.as_deref(), Some("did:plc:authority"));
            assert_eq!(catalog.get_entry(&id).await.unwrap().layer, Layer::Network);
            assert!(catalog.remove_network(&db, backend, &id).await.unwrap());
        }
    }

    #[tokio::test]
    async fn refresh_network_touches_only_network_rows_and_keeps_their_settings() {
        for (db, backend) in backends().await {
            let catalog = LexiconCatalog::detached();
            let net_id = unique("net");
            let man_id = unique("man");
            let mut write = network(record_json(&net_id), "did:plc:authority");
            write.target_collection = Some("com.example.target".into());
            catalog.put_network(&db, backend, write).await.unwrap();
            catalog
                .put_manual(&db, backend, manual(record_json(&man_id)))
                .await
                .unwrap();

            let mut changed = record_json(&net_id);
            changed["description"] = json!("v2");
            assert_eq!(
                catalog
                    .refresh_network(&db, backend, &net_id, changed)
                    .await
                    .unwrap(),
                Some(2)
            );
            let net = catalog.get_network(&net_id).await.unwrap();
            assert_eq!(net.parsed.raw["description"], "v2");
            assert_eq!(
                net.parsed.target_collection.as_deref(),
                Some("com.example.target")
            );
            assert_eq!(net.authority_did.as_deref(), Some("did:plc:authority"));

            assert_eq!(
                catalog
                    .refresh_network(&db, backend, &man_id, record_json(&man_id))
                    .await
                    .unwrap(),
                None
            );
            assert!(!catalog.remove_network(&db, backend, &man_id).await.unwrap());
            assert!(catalog.get(&man_id).await.is_some());

            catalog.remove(&db, backend, &net_id).await.unwrap();
            catalog.remove(&db, backend, &man_id).await.unwrap();
        }
    }

    #[tokio::test]
    async fn the_listing_merges_the_project_layer_and_keeps_rows_that_do_not_parse() {
        for (db, backend) in backends().await {
            let project = shared_empty();
            let catalog = LexiconCatalog::new(project.clone());
            let file_id = unique("file");
            let broken_id = unique("broken");
            lexicon_rows::insert_raw_for_test(&db, backend, &broken_id, "{not json").await;
            project.store(Arc::new(owning(&file_id, "src/lexicons/file.json")));

            catalog.load_from_db(&db).await.unwrap();
            assert!(
                catalog.get(&broken_id).await.is_none(),
                "an unparseable row is not served"
            );

            let listed = catalog.list(&db, backend).await.unwrap();
            let file = listed.iter().find(|l| l.id == file_id).unwrap();
            assert_eq!(file.layer, Layer::Project);
            assert_eq!(
                file.path.as_deref(),
                Some(std::path::Path::new("src/lexicons/file.json"))
            );
            assert!(file.created_at.is_none());
            let broken = listed.iter().find(|l| l.id == broken_id).unwrap();
            assert_eq!(broken.layer, Layer::Manual);
            assert!(broken.created_at.is_some());

            catalog.remove(&db, backend, &broken_id).await.unwrap();
        }
    }

    #[tokio::test]
    async fn load_from_db_reads_back_layers_and_authority_and_skips_unparseable_rows() {
        for (db, backend) in backends().await {
            let catalog = LexiconCatalog::detached();
            let man_id = unique("man");
            let net_id = unique("net");
            let broken_id = unique("broken");
            catalog
                .put_manual(&db, backend, manual(record_json(&man_id)))
                .await
                .unwrap();
            catalog
                .put_network(
                    &db,
                    backend,
                    network(record_json(&net_id), "did:plc:authority"),
                )
                .await
                .unwrap();
            lexicon_rows::insert_raw_for_test(&db, backend, &broken_id, "{not json").await;

            let fresh = LexiconCatalog::detached();
            fresh.load_from_db(&db).await.unwrap();
            assert_eq!(fresh.get_entry(&man_id).await.unwrap().layer, Layer::Manual);
            assert_eq!(
                fresh.get_entry(&net_id).await.unwrap().layer,
                Layer::Network
            );
            assert_eq!(
                fresh
                    .get_network(&net_id)
                    .await
                    .unwrap()
                    .authority_did
                    .as_deref(),
                Some("did:plc:authority")
            );
            assert!(fresh.get(&broken_id).await.is_none());
            assert!(
                fresh
                    .list(&db, backend)
                    .await
                    .unwrap()
                    .iter()
                    .any(|l| l.id == broken_id)
            );

            for id in [&man_id, &net_id, &broken_id] {
                catalog.remove(&db, backend, id).await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn a_manual_write_over_a_network_row_clears_its_authority() {
        for (db, backend) in backends().await {
            let catalog = LexiconCatalog::detached();
            let id = unique("convert");
            catalog
                .put_network(&db, backend, network(record_json(&id), "did:plc:authority"))
                .await
                .unwrap();
            catalog
                .put_manual(&db, backend, manual(record_json(&id)))
                .await
                .unwrap();

            let fresh = LexiconCatalog::detached();
            fresh.load_from_db(&db).await.unwrap();
            assert_eq!(fresh.get_entry(&id).await.unwrap().layer, Layer::Manual);
            assert!(fresh.get_network(&id).await.is_none());
            for listing in [
                catalog
                    .get_listing(&db, backend, &id)
                    .await
                    .unwrap()
                    .unwrap(),
                fresh.get_listing(&db, backend, &id).await.unwrap().unwrap(),
            ] {
                assert_eq!(listing.layer, Layer::Manual);
                assert!(listing.authority_did.is_none(), "{listing:?}");
                assert!(listing.last_fetched_at.is_none(), "{listing:?}");
            }

            catalog.remove(&db, backend, &id).await.unwrap();
        }
    }

    fn described(id: &str, description: &str) -> Value {
        json!({
            "lexicon": 1, "id": id, "description": description,
            "defs": { "main": { "type": "record", "key": "tid" } },
        })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writers_to_one_id_leave_memory_matching_the_table() {
        for (db, backend) in backends().await {
            let catalog = LexiconCatalog::detached();
            let id = unique("race");
            catalog
                .put_network(&db, backend, network(record_json(&id), "did:plc:authority"))
                .await
                .unwrap();

            for round in 0..40 {
                let (a, b, c) = tokio::join!(
                    catalog.put_manual(&db, backend, manual(described(&id, &format!("m{round}")))),
                    catalog.put_network(
                        &db,
                        backend,
                        network(described(&id, &format!("n{round}")), "did:plc:authority")
                    ),
                    catalog.refresh_network(
                        &db,
                        backend,
                        &id,
                        described(&id, &format!("r{round}"))
                    ),
                );
                a.unwrap();
                b.unwrap();
                c.unwrap();

                let fresh = LexiconCatalog::detached();
                fresh.load_from_db(&db).await.unwrap();
                let (mem, row) = (
                    catalog.get_entry(&id).await.unwrap(),
                    fresh.get_entry(&id).await.unwrap(),
                );
                assert_eq!(
                    (
                        mem.layer,
                        mem.value.revision,
                        mem.value.raw["description"].clone()
                    ),
                    (
                        row.layer,
                        row.value.revision,
                        row.value.raw["description"].clone()
                    ),
                    "round {round}: memory drifted from the table"
                );
            }

            catalog.remove(&db, backend, &id).await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_network_row_that_failed_to_load_is_listed_for_refresh_and_repaired_by_it() {
        for (db, backend) in backends().await {
            let catalog = LexiconCatalog::detached();
            let id = unique("corrupt");
            catalog
                .put_network(&db, backend, network(record_json(&id), "did:plc:authority"))
                .await
                .unwrap();
            lexicon_rows::corrupt_for_test(&db, backend, &id).await;

            let booted = LexiconCatalog::detached();
            booted.load_from_db(&db).await.unwrap();
            assert!(
                booted
                    .network_lexicons()
                    .await
                    .iter()
                    .all(|n| n.parsed.id != id),
                "memory never loaded it"
            );
            assert!(
                booted
                    .list_network(&db, backend)
                    .await
                    .unwrap()
                    .iter()
                    .any(|l| l.id == id),
                "the refresh set still has it"
            );

            let revision = booted
                .refresh_network(&db, backend, &id, described(&id, "repaired"))
                .await
                .unwrap();
            assert!(revision.is_some());
            let repaired = booted.get_network(&id).await.unwrap();
            assert_eq!(repaired.authority_did.as_deref(), Some("did:plc:authority"));

            let fresh = LexiconCatalog::detached();
            fresh.load_from_db(&db).await.unwrap();
            assert_eq!(fresh.get(&id).await.unwrap().raw["description"], "repaired");

            catalog.remove(&db, backend, &id).await.unwrap();
        }
    }
}
