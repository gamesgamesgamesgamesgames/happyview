//! Scripts, layered: a project file, then a dashboard row.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::Serialize;
use sqlx::AnyPool;

use super::{CatalogError, Layer, path_string, script_rows};
use crate::db::DatabaseBackend;
use crate::lua::scripts::{ParsedTrigger, ResolvedScript, TriggerKind};
use crate::project::{ProjectScript, ProjectSnapshot, ScriptBody, SharedSnapshot, shared_empty};

/// The chunk name of a script with no file. The wire's own default, so a row
/// resolved here and an input built without a chunk name agree.
pub const DASHBOARD_CHUNK_NAME: &str = crate::plugin::DEFAULT_CHUNK_NAME;

/// A record-event lookup's key within one batch: the snapshot it was
/// resolved in, by address, then the collection and the action.
type RecordEventKey = (usize, String, String);

tokio::task_local! {
    /// Record-event lookups already answered for the batch this task is
    /// applying (see [`ScriptCatalog::for_one_batch`]). Each holds its
    /// snapshot, so no other snapshot can take its address meanwhile.
    static RECORD_EVENT_LOOKUPS: RefCell<
        HashMap<RecordEventKey, (Arc<ProjectSnapshot>, Option<ResolvedScript>)>,
    >;
}

/// What the dashboard asks to store. `outbound_xrpcs` is derived from `body`
/// on every write, so no caller can store a stale list.
#[derive(Debug, Clone)]
pub struct ScriptWrite {
    pub id: String,
    pub script_type: String,
    pub body: String,
    pub description: Option<String>,
}

/// A stored row, as written.
#[derive(Debug, Clone, Serialize)]
pub struct ManualScript {
    pub id: String,
    pub script_type: String,
    pub body: String,
    pub description: Option<String>,
    pub outbound_xrpcs: Option<Vec<String>>,
    pub created_at: String,
    pub updated_at: String,
}

impl ManualScript {
    fn from_columns(c: script_rows::ScriptColumns) -> Self {
        let (id, script_type, body, description, outbound_json, created_at, updated_at) = c;
        // A corrupt list reads as "calls nothing", which is what the scope
        // check trusts, so the corruption must at least be visible.
        let outbound_xrpcs = outbound_json.and_then(|j| match serde_json::from_str(&j) {
            Ok(list) => Some(list),
            Err(e) => {
                tracing::warn!(script_id = %id, "stored outbound_xrpcs is not a JSON list: {e}");
                None
            }
        });
        Self {
            id,
            script_type,
            body,
            description,
            outbound_xrpcs,
            created_at,
            updated_at,
        }
    }
}

/// One script as the admin API lists it.
#[derive(Debug, Clone)]
pub struct ScriptListing {
    pub id: String,
    pub layer: Layer,
    pub path: Option<PathBuf>,
    /// A dashboard row sits under this project file. It is kept, untouched,
    /// and answers again when the file goes.
    pub shadows_stored: bool,
    pub script_type: String,
    pub body: String,
    pub description: Option<String>,
    pub outbound_xrpcs: Option<Vec<String>>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    /// Set for a compiled module's handler, which has no body to show or edit.
    pub module: Option<ModuleRow>,
}

/// A compiled module's handler, which the dashboard shows instead of a body.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModuleRow {
    pub module: String,
    /// The module's crate, project-relative, with `/` separators.
    pub path: String,
    /// The Rust function that handles the trigger.
    pub handler: String,
    pub hash: String,
}

/// The function name lives in the module's own section rather than in the
/// script, so saying which function handles a trigger needs the snapshot.
pub fn module_row(script: &ProjectScript, snap: &ProjectSnapshot) -> Option<ModuleRow> {
    let ScriptBody::Module { module, hash } = &script.body else {
        return None;
    };
    let handler = snap
        .script_modules
        .get(hash)
        .and_then(|m| m.handlers.iter().find(|h| h.trigger == script.trigger))
        .and_then(|h| h.function.clone())
        .unwrap_or_default();
    Some(ModuleRow {
        module: module.clone(),
        path: path_string(&script.path),
        handler,
        hash: hash.clone(),
    })
}

impl ScriptListing {
    fn from_manual(row: ManualScript) -> Self {
        Self {
            id: row.id,
            layer: Layer::Manual,
            path: None,
            shadows_stored: false,
            script_type: row.script_type,
            body: row.body,
            description: row.description,
            outbound_xrpcs: row.outbound_xrpcs,
            created_at: Some(row.created_at),
            updated_at: Some(row.updated_at),
            module: None,
        }
    }

    fn from_project(script: &ProjectScript, snap: &ProjectSnapshot, shadows_stored: bool) -> Self {
        Self {
            id: script.trigger.clone(),
            layer: Layer::Project,
            path: Some(script.path.clone()),
            shadows_stored,
            script_type: script.language_id.clone(),
            body: source_of(script),
            description: script.description.clone(),
            outbound_xrpcs: Some(script.outbound_xrpcs.clone()).filter(|v| !v.is_empty()),
            created_at: None,
            updated_at: None,
            module: module_row(script, snap),
        }
    }
}

fn source_of(script: &ProjectScript) -> String {
    match &script.body {
        ScriptBody::Source { source, .. } => source.to_string(),
        // A compiled module has no source; dispatch runs the module.
        ScriptBody::Module { .. } => String::new(),
    }
}

#[derive(Debug, Clone)]
pub struct ScriptCatalog {
    db: AnyPool,
    backend: DatabaseBackend,
    project: SharedSnapshot,
    /// How many times a lookup read the table.
    table_reads: Arc<AtomicUsize>,
}

impl ScriptCatalog {
    pub fn new(db: AnyPool, backend: DatabaseBackend, project: SharedSnapshot) -> Self {
        Self {
            db,
            backend,
            project,
            table_reads: Arc::default(),
        }
    }

    /// How many times a lookup has read the table rather than answering from
    /// the project or a batch's earlier lookup.
    pub fn table_reads(&self) -> usize {
        self.table_reads.load(Ordering::Relaxed)
    }

    /// Runs `fut` with record-event lookups remembered for its length: one
    /// batch of events resolves each collection's hook once, not once per
    /// record. A row saved meanwhile reaches the next batch.
    pub async fn for_one_batch<F: Future>(fut: F) -> F::Output {
        RECORD_EVENT_LOOKUPS
            .scope(RefCell::new(HashMap::new()), fut)
            .await
    }

    /// A catalog with an empty project layer, for a process that runs no
    /// project, such as the codemod CLI.
    pub fn detached(db: AnyPool, backend: DatabaseBackend) -> Self {
        Self::new(db, backend, shared_empty())
    }

    pub fn check_writable(&self, trigger_id: &str) -> Result<(), CatalogError> {
        match self.project.load().scripts.get(trigger_id) {
            Some(script) => Err(CatalogError::ProjectOwned {
                path: script.path.clone(),
            }),
            None => Ok(()),
        }
    }

    /// Looks a trigger up in `snap` and then in the table. The caller passes
    /// the snapshot it pinned when its run began, never the live one, so a
    /// swap mid-run cannot change which script the run is executing.
    ///
    /// The row's language is not a filter: a trigger bound to a script must
    /// resolve to that script whether or not its interpreter is installed, so
    /// the answer is the dispatcher's refusal rather than silence.
    pub async fn resolve_in(
        &self,
        snap: &Arc<ProjectSnapshot>,
        trigger: &ParsedTrigger,
    ) -> Option<ResolvedScript> {
        let trigger_id = trigger.id();
        if let Some(script) = snap.scripts.get(&trigger_id) {
            return Some(ResolvedScript {
                id: trigger_id,
                trigger: trigger.kind,
                script_type: script.language_id.clone(),
                body: source_of(script),
                layer: Layer::Project,
                path: Some(script.path.clone()),
                chunk_name: path_string(&script.path),
                outbound_xrpcs: script.outbound_xrpcs.clone(),
                snapshot: Arc::clone(snap),
            });
        }
        self.table_reads.fetch_add(1, Ordering::Relaxed);
        match script_rows::get(&self.db, self.backend, &trigger_id).await {
            Ok(Some(columns)) => {
                let row = ManualScript::from_columns(columns);
                Some(ResolvedScript {
                    id: row.id,
                    trigger: trigger.kind,
                    script_type: row.script_type,
                    body: row.body,
                    layer: Layer::Manual,
                    path: None,
                    chunk_name: DASHBOARD_CHUNK_NAME.to_string(),
                    outbound_xrpcs: row.outbound_xrpcs.unwrap_or_default(),
                    snapshot: Arc::clone(snap),
                })
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(trigger_id, "scripts lookup failed: {e}");
                None
            }
        }
    }

    /// `record.<action>:<nsid>` first, then `record.index:<nsid>`, each looked
    /// up through every layer.
    pub async fn resolve_record_event_in(
        &self,
        snap: &Arc<ProjectSnapshot>,
        nsid: &str,
        action: &str,
    ) -> Option<ResolvedScript> {
        let key = (
            Arc::as_ptr(snap) as usize,
            nsid.to_string(),
            action.to_string(),
        );
        if let Ok(Some(known)) =
            RECORD_EVENT_LOOKUPS.try_with(|known| known.borrow().get(&key).map(|(_, s)| s.clone()))
        {
            return known;
        }
        let resolved = self.lookup_record_event(snap, nsid, action).await;
        let _ = RECORD_EVENT_LOOKUPS.try_with(|known| {
            known
                .borrow_mut()
                .insert(key, (Arc::clone(snap), resolved.clone()))
        });
        resolved
    }

    async fn lookup_record_event(
        &self,
        snap: &Arc<ProjectSnapshot>,
        nsid: &str,
        action: &str,
    ) -> Option<ResolvedScript> {
        let action_kind = match action {
            "create" => Some(TriggerKind::RecordCreate),
            "update" => Some(TriggerKind::RecordUpdate),
            "delete" => Some(TriggerKind::RecordDelete),
            _ => None,
        };
        if let Some(kind) = action_kind
            && let Some(s) = self.resolve_in(snap, &ParsedTrigger::new(kind, nsid)).await
        {
            return Some(s);
        }
        self.resolve_in(snap, &ParsedTrigger::new(TriggerKind::RecordIndex, nsid))
            .await
    }

    /// Stores a dashboard row. `true` when the row is new.
    pub async fn put_manual(
        &self,
        db: &AnyPool,
        backend: DatabaseBackend,
        row: ScriptWrite,
    ) -> Result<bool, CatalogError> {
        self.check_writable(&row.id)?;
        let outbound = crate::lua_analysis::extract_outbound_xrpcs(&row.body);
        let outbound_json = if outbound.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&outbound).map_err(|e| {
                CatalogError::Invalid(format!("failed to serialize outbound xrpcs: {e}"))
            })?)
        };
        let created = !script_rows::exists(db, backend, &row.id).await?;
        script_rows::upsert(
            db,
            backend,
            &row.id,
            &row.script_type,
            &row.body,
            row.description.as_deref(),
            outbound_json.as_deref(),
        )
        .await?;
        Ok(created)
    }

    pub async fn remove(
        &self,
        db: &AnyPool,
        backend: DatabaseBackend,
        trigger_id: &str,
    ) -> Result<bool, CatalogError> {
        self.check_writable(trigger_id)?;
        Ok(script_rows::delete(db, backend, trigger_id).await? > 0)
    }

    pub async fn get_manual(&self, id: &str) -> Result<Option<ManualScript>, CatalogError> {
        Ok(script_rows::get(&self.db, self.backend, id)
            .await?
            .map(ManualScript::from_columns))
    }

    pub async fn list_manual(
        &self,
        suffix: Option<&str>,
    ) -> Result<Vec<ManualScript>, CatalogError> {
        Ok(script_rows::list(&self.db, self.backend, suffix)
            .await?
            .into_iter()
            .map(ManualScript::from_columns)
            .collect())
    }

    /// The script answering for `id` under the current snapshot.
    pub async fn get(&self, id: &str) -> Result<Option<ScriptListing>, CatalogError> {
        let project = self.project.load_full();
        let row = self.get_manual(id).await?;
        Ok(match project.scripts.get(id) {
            Some(script) => Some(ScriptListing::from_project(script, &project, row.is_some())),
            None => row.map(ScriptListing::from_manual),
        })
    }

    /// Every script answering under the current snapshot, by id.
    pub async fn list(&self, suffix: Option<&str>) -> Result<Vec<ScriptListing>, CatalogError> {
        let project = self.project.load_full();
        let mut out: BTreeMap<String, ScriptListing> = self
            .list_manual(suffix)
            .await?
            .into_iter()
            .map(|row| (row.id.clone(), ScriptListing::from_manual(row)))
            .collect();
        let wanted = suffix.map(|s| format!(":{s}"));
        for (id, script) in &project.scripts {
            if wanted.as_deref().is_some_and(|w| !id.ends_with(w)) {
                continue;
            }
            let shadows_stored = out.contains_key(id);
            out.insert(
                id.clone(),
                ScriptListing::from_project(script, &project, shadows_stored),
            );
        }
        Ok(out.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{ProjectScript, ProjectSnapshot, ScriptBody, shared_empty};
    use std::sync::Arc;

    async fn backends() -> Vec<(AnyPool, DatabaseBackend)> {
        crate::test_support::test_pools().await
    }

    fn unique_nsid() -> String {
        let tag = uuid::Uuid::new_v4().simple().to_string();
        format!("com.example.s{}.thing", &tag[..10])
    }

    fn write(id: &str, body: &str) -> ScriptWrite {
        ScriptWrite {
            id: id.to_string(),
            script_type: "lua".to_string(),
            body: body.to_string(),
            description: None,
        }
    }

    fn snapshot_with(scripts: &[(&str, &str, &str)]) -> ProjectSnapshot {
        let mut snap = ProjectSnapshot::empty();
        for (trigger, body, path) in scripts {
            snap.scripts.insert(
                trigger.to_string(),
                ProjectScript {
                    trigger: trigger.to_string(),
                    path: PathBuf::from(path),
                    language_id: "lua".to_string(),
                    body: ScriptBody::Source {
                        source: Arc::from(*body),
                        artifact: None,
                    },
                    description: None,
                    outbound_xrpcs: vec!["com.example.other".to_string()],
                },
            );
        }
        snap
    }

    #[tokio::test]
    async fn resolve_in_answers_from_the_snapshot_it_is_given_not_the_live_one() {
        for (db, backend) in backends().await {
            let project = shared_empty();
            let catalog = ScriptCatalog::new(db.clone(), backend, project.clone());
            let nsid = unique_nsid();
            let trigger_id = format!("xrpc.query:{nsid}");
            project.store(Arc::new(snapshot_with(&[(
                &trigger_id,
                "live",
                "src/live.lua",
            )])));
            let pinned = Arc::new(snapshot_with(&[(
                &trigger_id,
                "pinned",
                "src/lexicons/pinned.lua",
            )]));

            let trigger = ParsedTrigger::new(TriggerKind::XrpcQuery, nsid.as_str());
            let resolved = catalog.resolve_in(&pinned, &trigger).await.unwrap();
            assert_eq!(resolved.body, "pinned");
            assert_eq!(resolved.layer, Layer::Project);
            assert_eq!(resolved.chunk_name, "src/lexicons/pinned.lua");
            assert_eq!(
                resolved.outbound_xrpcs,
                vec!["com.example.other".to_string()]
            );
            assert!(
                Arc::ptr_eq(&resolved.snapshot, &pinned),
                "the run carries the snapshot it pinned"
            );
        }
    }

    #[tokio::test]
    async fn a_project_script_shadows_a_row_and_the_row_answers_again_when_the_file_goes() {
        for (db, backend) in backends().await {
            let catalog = ScriptCatalog::detached(db.clone(), backend);
            let nsid = unique_nsid();
            let trigger_id = format!("xrpc.query:{nsid}");
            assert!(
                catalog
                    .put_manual(&db, backend, write(&trigger_id, "row"))
                    .await
                    .unwrap()
            );
            let trigger = ParsedTrigger::new(TriggerKind::XrpcQuery, nsid.as_str());

            let from_file = Arc::new(snapshot_with(&[(
                &trigger_id,
                "file",
                "src/lexicons/thing.lua",
            )]));
            assert_eq!(
                catalog.resolve_in(&from_file, &trigger).await.unwrap().body,
                "file"
            );

            let empty = Arc::new(ProjectSnapshot::empty());
            let resolved = catalog.resolve_in(&empty, &trigger).await.unwrap();
            assert_eq!(
                (resolved.body.as_str(), resolved.layer),
                ("row", Layer::Manual)
            );
            assert_eq!(resolved.chunk_name, DASHBOARD_CHUNK_NAME);
            assert!(resolved.path.is_none());
            assert!(
                Arc::ptr_eq(&resolved.snapshot, &empty),
                "a row hit carries the pinned snapshot too"
            );

            catalog.remove(&db, backend, &trigger_id).await.unwrap();
        }
    }

    /// The action trigger wins over the index trigger whichever layer holds
    /// each; within one trigger the project wins.
    #[tokio::test]
    async fn the_record_cascade_prefers_the_action_trigger_across_layers() {
        for (db, backend) in backends().await {
            let catalog = ScriptCatalog::detached(db.clone(), backend);
            let nsid = unique_nsid();
            catalog
                .put_manual(
                    &db,
                    backend,
                    write(&format!("record.create:{nsid}"), "row create"),
                )
                .await
                .unwrap();
            let snap = Arc::new(snapshot_with(&[(
                &format!("record.index:{nsid}"),
                "file index",
                "src/thing.index.lua",
            )]));

            let create = catalog
                .resolve_record_event_in(&snap, &nsid, "create")
                .await
                .unwrap();
            assert_eq!(create.body, "row create");
            assert_eq!(create.trigger, TriggerKind::RecordCreate);
            let update = catalog
                .resolve_record_event_in(&snap, &nsid, "update")
                .await
                .unwrap();
            assert_eq!(update.body, "file index");
            assert_eq!(update.trigger, TriggerKind::RecordIndex);

            catalog
                .remove(&db, backend, &format!("record.create:{nsid}"))
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn put_manual_reports_creation_and_derives_outbound_xrpcs() {
        for (db, backend) in backends().await {
            let catalog = ScriptCatalog::detached(db.clone(), backend);
            let id = format!("xrpc.procedure:{}", unique_nsid());
            let body = r#"xrpc.query("com.example.other", {})"#;

            assert!(
                catalog
                    .put_manual(&db, backend, write(&id, body))
                    .await
                    .unwrap()
            );
            assert!(
                !catalog
                    .put_manual(&db, backend, write(&id, body))
                    .await
                    .unwrap()
            );
            let row = catalog.get_manual(&id).await.unwrap().unwrap();
            assert_eq!(
                row.outbound_xrpcs,
                Some(vec!["com.example.other".to_string()])
            );

            catalog
                .put_manual(&db, backend, write(&id, "return {}"))
                .await
                .unwrap();
            assert_eq!(
                catalog
                    .get_manual(&id)
                    .await
                    .unwrap()
                    .unwrap()
                    .outbound_xrpcs,
                None
            );
            catalog.remove(&db, backend, &id).await.unwrap();
        }
    }

    /// A row stored before the NSID rules tightened is still editable, so a
    /// write must not run the trigger grammar.
    #[tokio::test]
    async fn put_manual_accepts_a_trigger_id_the_grammar_now_refuses() {
        for (db, backend) in backends().await {
            let catalog = ScriptCatalog::detached(db.clone(), backend);
            let tag = &uuid::Uuid::new_v4().simple().to_string()[..10];
            let id = format!("record.create:legacy_{tag}");
            assert!(ParsedTrigger::parse(&id).is_err());
            catalog
                .put_manual(&db, backend, write(&id, "return {}"))
                .await
                .unwrap();
            assert!(catalog.remove(&db, backend, &id).await.unwrap());
        }
    }

    #[tokio::test]
    async fn writes_refuse_a_project_owned_trigger_and_keep_the_row() {
        for (db, backend) in backends().await {
            let project = shared_empty();
            let catalog = ScriptCatalog::new(db.clone(), backend, project.clone());
            let id = format!("xrpc.query:{}", unique_nsid());
            catalog
                .put_manual(&db, backend, write(&id, "row"))
                .await
                .unwrap();
            project.store(Arc::new(snapshot_with(&[(
                &id,
                "file",
                "src/lexicons/thing.lua",
            )])));

            assert!(matches!(
                catalog.put_manual(&db, backend, write(&id, "edit")).await,
                Err(CatalogError::ProjectOwned { .. })
            ));
            assert!(matches!(
                catalog.remove(&db, backend, &id).await,
                Err(CatalogError::ProjectOwned { .. })
            ));
            let listing = catalog.get(&id).await.unwrap().unwrap();
            assert_eq!(
                (listing.layer, listing.body.as_str()),
                (Layer::Project, "file")
            );
            assert!(listing.shadows_stored);
            assert_eq!(catalog.get_manual(&id).await.unwrap().unwrap().body, "row");

            project.store(Arc::new(ProjectSnapshot::empty()));
            assert!(catalog.remove(&db, backend, &id).await.unwrap());
        }
    }

    /// The SQL half matches `LIKE '%:<suffix>'`; the project half has to
    /// match the same set, including not matching a longer segment's tail.
    #[tokio::test]
    async fn the_suffix_filter_matches_the_same_ids_in_both_layers() {
        for (db, backend) in backends().await {
            let project = shared_empty();
            let catalog = ScriptCatalog::new(db.clone(), backend, project.clone());
            let nsid = unique_nsid();
            let longer = format!("{nsid}x");
            catalog
                .put_manual(&db, backend, write(&format!("record.index:{nsid}"), "row"))
                .await
                .unwrap();
            catalog
                .put_manual(
                    &db,
                    backend,
                    write(&format!("record.index:{longer}"), "row"),
                )
                .await
                .unwrap();
            // Tail equals the suffix but is not preceded by a colon.
            let decoy = format!("x{nsid}");
            catalog
                .put_manual(&db, backend, write(&format!("record.index:{decoy}"), "row"))
                .await
                .unwrap();
            project.store(Arc::new(snapshot_with(&[
                (&format!("xrpc.query:{nsid}"), "file", "src/a.lua"),
                (&format!("xrpc.query:{longer}"), "file", "src/b.lua"),
                (&format!("xrpc.query:{decoy}"), "file", "src/c.lua"),
            ])));

            let ids: Vec<String> = catalog
                .list(Some(nsid.as_str()))
                .await
                .unwrap()
                .into_iter()
                .map(|l| l.id)
                .collect();
            assert_eq!(
                ids,
                vec![format!("record.index:{nsid}"), format!("xrpc.query:{nsid}")]
            );

            project.store(Arc::new(ProjectSnapshot::empty()));
            catalog
                .remove(&db, backend, &format!("record.index:{nsid}"))
                .await
                .unwrap();
            catalog
                .remove(&db, backend, &format!("record.index:{longer}"))
                .await
                .unwrap();
            catalog
                .remove(&db, backend, &format!("record.index:{decoy}"))
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn a_corrupt_outbound_column_reads_as_none_rather_than_failing() {
        for (db, backend) in backends().await {
            let catalog = ScriptCatalog::detached(db.clone(), backend);
            let id = format!("xrpc.procedure:{}", unique_nsid());
            catalog
                .put_manual(&db, backend, write(&id, "return {}"))
                .await
                .unwrap();
            let sql = crate::db::adapt_sql(
                "UPDATE happyview_scripts SET outbound_xrpcs = ? WHERE id = ?",
                backend,
            );
            crate::db::query(&sql)
                .bind("not json")
                .bind(&id)
                .execute(&db)
                .await
                .unwrap();

            assert_eq!(
                catalog
                    .get_manual(&id)
                    .await
                    .unwrap()
                    .unwrap()
                    .outbound_xrpcs,
                None
            );
            catalog.remove(&db, backend, &id).await.unwrap();
        }
    }

    #[test]
    fn a_module_handler_lists_its_module_path_function_and_hash() {
        use crate::project::script_module::{ModuleHandler, ScriptModule};

        let mut snap = ProjectSnapshot::empty();
        let manifest: crate::plugin::PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "module:registry", "name": "registry", "version": "0.0.0",
            "api_version": "2", "plugin_type": "interpreter", "language_id": "rust",
        }))
        .unwrap();
        let plugin = crate::plugin::LoadedPlugin {
            info: manifest.clone().into(),
            source: crate::plugin::PluginSource::File { path: "x".into() },
            wasm_bytes: Vec::new(),
            manifest: Some(manifest),
        };
        snap.script_modules.insert(
            "abc123".into(),
            Arc::new(ScriptModule {
                name: "registry".into(),
                source_path: "modules/registry".into(),
                artifact_path: ".happyview/build/registry.wasm".into(),
                hash: "abc123".into(),
                handlers: vec![ModuleHandler {
                    trigger: "xrpc.query:app.test.echo".into(),
                    function: Some("echo".into()),
                }],
                plugin: Arc::new(plugin),
            }),
        );
        let script = ProjectScript {
            trigger: "xrpc.query:app.test.echo".into(),
            path: "modules/registry".into(),
            language_id: "rust".into(),
            body: ScriptBody::Module {
                module: "registry".into(),
                hash: "abc123".into(),
            },
            description: None,
            outbound_xrpcs: vec![],
        };
        let expected = ModuleRow {
            module: "registry".into(),
            path: "modules/registry".into(),
            handler: "echo".into(),
            hash: "abc123".into(),
        };
        assert_eq!(module_row(&script, &snap), Some(expected.clone()));
        let listing = ScriptListing::from_project(&script, &snap, false);
        assert_eq!(listing.module, Some(expected));
        assert_eq!(listing.body, "");
    }
}
