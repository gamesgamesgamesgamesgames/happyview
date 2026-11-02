//! Keeping the plugins a dev server runs in step with `PLUGIN_DIR`.
//!
//! `hpy happyview dev` places every plugin the lock pins in `PLUGIN_DIR`
//! before boot, and `hpy happyview add` places them again after it updates
//! the lock. Boot loads that directory once; this loads what was added to it
//! since, and unloads what was removed, through the same calls boot makes.
//! Plugins installed from a URL or from the database are never touched.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::plugin::graph::GraphError;
use crate::plugin::{LoadedPlugin, PluginRegistry, PluginSource, loader};

/// A plugin as the sync compares it: the bytes and version that decide
/// whether a placed copy is new.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint<'a> {
    pub id: &'a str,
    pub version: &'a str,
    pub wasm: &'a [u8],
}

/// One entry of `PLUGIN_DIR` as read this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listed<'a> {
    Loaded(Fingerprint<'a>),
    /// An entry that did not load, perhaps because it is still being
    /// written. Whatever was installed from its path stays installed.
    Failed(&'a Path),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Ids to install: new, or with other bytes or another version.
    pub install: BTreeSet<String>,
    /// Ids no longer in the directory.
    pub remove: BTreeSet<String>,
    /// Ids in the directory that are installed from somewhere else, a URL
    /// or the database. Those are never replaced live.
    pub shadowed: BTreeSet<String>,
}

/// What it takes to bring the file plugins installed from the directory,
/// with the path each came from, in line with what the directory holds now.
/// `elsewhere` is every other installed id.
pub fn plan(
    installed: &[(Fingerprint<'_>, &Path)],
    elsewhere: &BTreeSet<&str>,
    listed: &[Listed<'_>],
) -> Plan {
    // A later entry with the same id wins, as it does in `register_all`.
    let mut present: BTreeMap<&str, Fingerprint<'_>> = BTreeMap::new();
    let mut failed: BTreeSet<&Path> = BTreeSet::new();
    for entry in listed {
        match entry {
            Listed::Loaded(fp) => {
                present.insert(fp.id, *fp);
            }
            Listed::Failed(path) => {
                failed.insert(path);
            }
        }
    }
    let by_id: BTreeMap<&str, &Fingerprint<'_>> =
        installed.iter().map(|(fp, _)| (fp.id, fp)).collect();

    let remove = installed
        .iter()
        .filter(|(fp, path)| !present.contains_key(fp.id) && !failed.contains(*path))
        .map(|(fp, _)| fp.id.to_string())
        .collect();
    let (shadowed, present): (Vec<_>, Vec<_>) = present
        .into_values()
        .partition(|fp| elsewhere.contains(fp.id));
    let shadowed = shadowed.iter().map(|fp| fp.id.to_string()).collect();
    let install = present
        .iter()
        .filter(|fp| {
            by_id
                .get(fp.id)
                .is_none_or(|old| old.version != fp.version || old.wasm != fp.wasm)
        })
        .map(|fp| fp.id.to_string())
        .collect();
    Plan {
        install,
        remove,
        shadowed,
    }
}

fn fingerprint(plugin: &LoadedPlugin) -> Fingerprint<'_> {
    Fingerprint {
        id: &plugin.info.id,
        version: &plugin.info.version,
        wasm: &plugin.wasm_bytes,
    }
}

/// Bring the registry's file plugins from `dir` in line with `dir`. Answers
/// the line that says what changed, or `None` when nothing did.
///
/// Loading goes through `register_all`, as boot does, so a plugin written
/// here gets the same `happyview_plugins` row boot gives it. A removal only
/// unloads, as a plugin missing at boot is simply not loaded: boot never
/// deletes a row either.
pub async fn sync(registry: &PluginRegistry, dir: &Path) -> Option<String> {
    let mut loaded: Vec<LoadedPlugin> = Vec::new();
    let mut failed: Vec<(PathBuf, String)> = Vec::new();
    for path in loader::plugin_dirs_in(dir) {
        match loader::load_from_file(&path).await {
            Ok(plugin) => loaded.push(plugin),
            Err(e) => failed.push((path, e.to_string())),
        }
    }

    let (installed, others): (Vec<_>, Vec<_>) =
        registry.list().await.into_iter().partition(
            |p| matches!(&p.source, PluginSource::File { path } if path.starts_with(dir)),
        );
    let elsewhere: BTreeSet<&str> = others.iter().map(|p| p.info.id.as_str()).collect();
    let installed_fps: Vec<(Fingerprint<'_>, &Path)> = installed
        .iter()
        .map(|p| {
            let PluginSource::File { path } = &p.source else {
                unreachable!("filtered to file plugins")
            };
            (fingerprint(p), path.as_path())
        })
        .collect();
    let mut listed: Vec<Listed<'_>> = loaded
        .iter()
        .map(|p| Listed::Loaded(fingerprint(p)))
        .collect();
    listed.extend(failed.iter().map(|(path, _)| Listed::Failed(path)));
    let plan = plan(&installed_fps, &elsewhere, &listed);
    let was_installed: BTreeSet<String> = installed.iter().map(|p| p.info.id.clone()).collect();
    drop(installed_fps);
    drop(installed);
    drop(elsewhere);
    drop(others);

    let mut parts = Vec::new();
    for (path, e) in &failed {
        parts.push(format!("could not load {}: {e}", path.display()));
    }
    for id in &plan.shadowed {
        parts.push(format!(
            "left {id} as installed from elsewhere; restart to load the copy in {}",
            dir.display()
        ));
    }

    // Dependents first: each pass removes whatever nothing left still
    // needs, so a library and the plugin using it can go in one batch.
    let mut pending = plan.remove.clone();
    let mut removed = Vec::new();
    loop {
        let mut kept = BTreeMap::new();
        let before = pending.len();
        for id in std::mem::take(&mut pending) {
            match registry.uninstall(&id, false).await {
                Ok(_) => removed.push(id),
                Err(e) => {
                    kept.insert(id.clone(), e);
                    pending.insert(id);
                }
            }
        }
        if pending.is_empty() || pending.len() == before {
            for (id, e) in kept {
                parts.push(match e {
                    GraphError::HasDependents { dependents, .. } => format!(
                        "kept {id}, which {} still needs, though it left {}",
                        dependents.join(", "),
                        dir.display()
                    ),
                    other => format!("could not remove {id}: {other}"),
                });
            }
            break;
        }
    }

    // In directory order, so the line reads the same every time.
    let mut to_install: Vec<LoadedPlugin> = Vec::new();
    let mut seen = BTreeSet::new();
    for plugin in loaded.into_iter().rev() {
        if plan.install.contains(&plugin.info.id) && seen.insert(plugin.info.id.clone()) {
            to_install.push(plugin);
        }
    }
    to_install.reverse();
    let summary: Vec<(String, String)> = to_install
        .iter()
        .map(|p| (p.info.id.clone(), p.info.version.clone()))
        .collect();
    let failures: BTreeMap<String, GraphError> = registry
        .register_all(to_install)
        .await
        .into_iter()
        .collect();
    for (id, version) in summary {
        match failures.get(&id) {
            Some(e) => parts.push(format!("could not load {id} {version}: {e}")),
            None if was_installed.contains(&id) => parts.push(format!("updated {id} {version}")),
            None => parts.push(format!("loaded {id} {version}")),
        }
    }
    parts.extend(removed.into_iter().map(|id| format!("removed {id}")));

    (!parts.is_empty()).then(|| format!("plugins: {}", parts.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp<'a>(id: &'a str, version: &'a str, wasm: &'a [u8]) -> Fingerprint<'a> {
        Fingerprint { id, version, wasm }
    }

    fn ids(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn new_changed_and_missing_plugins_are_planned() {
        let a = Path::new("/p/a");
        let b = Path::new("/p/b");
        let c = Path::new("/p/c");
        let installed = [
            (fp("a", "1.0.0", b"a"), a),
            (fp("b", "1.0.0", b"b"), b),
            (fp("c", "1.0.0", b"c"), c),
        ];
        let listed = [
            // Unchanged.
            Listed::Loaded(fp("a", "1.0.0", b"a")),
            // New bytes under the same version, as a rebuilt local copy has.
            Listed::Loaded(fp("b", "1.0.0", b"b2")),
            // New.
            Listed::Loaded(fp("d", "0.1.0", b"d")),
        ];
        assert_eq!(
            plan(&installed, &BTreeSet::new(), &listed),
            Plan {
                install: ids(&["b", "d"]),
                remove: ids(&["c"]),
                ..Plan::default()
            }
        );
    }

    #[test]
    fn a_new_version_is_installed_even_with_the_same_bytes() {
        let installed = [(fp("a", "1.0.0", b"a"), Path::new("/p/a"))];
        let listed = [Listed::Loaded(fp("a", "1.0.1", b"a"))];
        assert_eq!(
            plan(&installed, &BTreeSet::new(), &listed).install,
            ids(&["a"])
        );
    }

    #[test]
    fn an_unchanged_directory_plans_nothing() {
        let installed = [(fp("a", "1.0.0", b"a"), Path::new("/p/a"))];
        let listed = [Listed::Loaded(fp("a", "1.0.0", b"a"))];
        assert_eq!(plan(&installed, &BTreeSet::new(), &listed), Plan::default());
        assert_eq!(plan(&[], &BTreeSet::new(), &[]), Plan::default());
    }

    /// An entry caught half-written keeps what its path last loaded rather
    /// than unloading it until the next event.
    #[test]
    fn an_entry_that_fails_to_load_keeps_what_it_held() {
        let installed = [
            (fp("a", "1.0.0", b"a"), Path::new("/p/a")),
            (fp("b", "1.0.0", b"b"), Path::new("/p/b")),
        ];
        let listed = [Listed::Failed(Path::new("/p/a"))];
        assert_eq!(
            plan(&installed, &BTreeSet::new(), &listed),
            Plan {
                remove: ids(&["b"]),
                ..Plan::default()
            }
        );
    }

    /// A plugin installed from a URL or the database keeps its place, even
    /// when the directory holds one with its id.
    #[test]
    fn a_plugin_from_elsewhere_is_not_replaced() {
        let elsewhere = BTreeSet::from(["a"]);
        let listed = [Listed::Loaded(fp("a", "1.0.0", b"a"))];
        assert_eq!(
            plan(&[], &elsewhere, &listed),
            Plan {
                shadowed: ids(&["a"]),
                ..Plan::default()
            }
        );
    }

    fn place(dir: &Path, entry: &str, manifest: serde_json::Value, wat: &str) {
        let at = dir.join(entry);
        std::fs::create_dir_all(&at).unwrap();
        std::fs::write(at.join("manifest.json"), manifest.to_string()).unwrap();
        std::fs::write(at.join("plugin.wasm"), wat::parse_str(wat).unwrap()).unwrap();
    }

    fn library(id: &str, version: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "name": id, "version": version, "api_version": "2",
            "plugin_type": "library", "capabilities": [], "wasm_file": "plugin.wasm",
        })
    }

    const MODULE: &str = r#"(module (memory (export "memory") 1))"#;

    async fn file_rows(pool: &sqlx::AnyPool) -> Vec<(String, String)> {
        crate::db::query_as("SELECT id, source FROM happyview_plugins ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// Placing, replacing and removing entries loads, updates and unloads
    /// them, writing the same rows boot writes; a URL plugin is untouched.
    #[tokio::test]
    async fn the_registry_follows_the_directory() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let registry = PluginRegistry::with_db(pool.clone(), crate::db::DatabaseBackend::Sqlite);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plugins");

        // A missing directory is an empty one.
        assert_eq!(sync(&registry, &dir).await, None);

        let url_manifest: crate::plugin::PluginManifest =
            serde_json::from_value(library("from-url", "1.0.0")).unwrap();
        registry
            .register(LoadedPlugin {
                info: url_manifest.clone().into(),
                source: PluginSource::Url {
                    url: "https://example.invalid/m.json".into(),
                    sha256: None,
                    registry: None,
                },
                wasm_bytes: vec![],
                manifest: Some(url_manifest),
            })
            .await;

        place(
            &dir,
            "happyview-db",
            library("happyview-db", "1.0.0"),
            MODULE,
        );
        place(
            &dir,
            "happyview-sql",
            library("happyview-sql", "0.3.0"),
            MODULE,
        );
        assert_eq!(
            sync(&registry, &dir).await.as_deref(),
            Some("plugins: loaded happyview-db 1.0.0; loaded happyview-sql 0.3.0")
        );
        assert_eq!(
            file_rows(&pool).await,
            vec![
                ("from-url".into(), "url".into()),
                ("happyview-db".into(), "file".into()),
                ("happyview-sql".into(), "file".into()),
            ]
        );
        assert_eq!(sync(&registry, &dir).await, None, "nothing changed");

        std::fs::remove_dir_all(dir.join("happyview-sql")).unwrap();
        place(
            &dir,
            "happyview-db",
            library("happyview-db", "1.0.1"),
            MODULE,
        );
        assert_eq!(
            sync(&registry, &dir).await.as_deref(),
            Some("plugins: updated happyview-db 1.0.1; removed happyview-sql")
        );
        assert_eq!(
            registry.get("happyview-db").await.unwrap().info.version,
            "1.0.1"
        );
        assert!(registry.get("happyview-sql").await.is_none());
        assert!(registry.get("from-url").await.is_some());

        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            sync(&registry, &dir).await.as_deref(),
            Some("plugins: removed happyview-db")
        );
        assert!(registry.get("from-url").await.is_some());

        // A copy of the URL plugin placed here does not replace it.
        place(&dir, "from-url", library("from-url", "2.0.0"), MODULE);
        let line = sync(&registry, &dir).await.unwrap();
        assert!(
            line.starts_with("plugins: left from-url as installed from elsewhere"),
            "{line}"
        );
        assert_eq!(
            registry.get("from-url").await.unwrap().info.version,
            "1.0.0"
        );
    }

    /// A library still needed by a plugin outside the directory stays, and
    /// the line says why.
    #[tokio::test]
    async fn a_library_with_dependents_is_kept() {
        let registry = PluginRegistry::new();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plugins");
        place(&dir, "base", library("base", "1.0.0"), MODULE);
        sync(&registry, &dir).await;

        let mut user = library("user", "1.0.0");
        user["dependencies"] = serde_json::json!([{ "id": "base" }]);
        let user: crate::plugin::PluginManifest = serde_json::from_value(user).unwrap();
        registry
            .install(LoadedPlugin {
                info: user.clone().into(),
                source: PluginSource::Url {
                    url: "https://example.invalid/u.json".into(),
                    sha256: None,
                    registry: None,
                },
                wasm_bytes: vec![],
                manifest: Some(user),
            })
            .await
            .unwrap();

        std::fs::remove_dir_all(dir.join("base")).unwrap();
        let line = sync(&registry, &dir).await.unwrap();
        assert!(
            line.starts_with("plugins: kept base, which user still needs"),
            "{line}"
        );
        assert!(registry.get("base").await.is_some());
    }

    /// A plugin and the library it uses, removed together, both go.
    #[tokio::test]
    async fn a_library_and_its_dependent_leave_together() {
        let registry = PluginRegistry::new();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plugins");
        place(&dir, "base", library("base", "1.0.0"), MODULE);
        let mut user = library("user", "1.0.0");
        user["dependencies"] = serde_json::json!([{ "id": "base" }]);
        place(&dir, "user", user, MODULE);
        assert_eq!(
            sync(&registry, &dir).await.as_deref(),
            Some("plugins: loaded base 1.0.0; loaded user 1.0.0")
        );

        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            sync(&registry, &dir).await.as_deref(),
            Some("plugins: removed user; removed base")
        );
    }
}
