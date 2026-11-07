//! What a deploy changes, kind by kind, for the client to show before it is
//! confirmed and after it lands.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::config::project::ProjectConfig;
use crate::project::ProjectSnapshot;
use crate::project::build::{same_lexicon, same_module, same_script};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct KindChanges {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

impl KindChanges {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Changes {
    /// By NSID.
    pub lexicons: KindChanges,
    /// By trigger id, whatever the trigger.
    pub scripts: KindChanges,
    /// Shared source modules by project path, compiled `[modules.<name>]`
    /// by name. A path always holds a `/` and a name never does.
    pub modules: KindChanges,
    /// `[network_lexicons] include`. Only ever added or removed: a removed
    /// one is no longer declared, and its row stays as a dashboard one.
    pub network_lexicons: KindChanges,
    /// `[labels] subscribe`, on the same terms.
    pub labelers: KindChanges,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.lexicons.is_empty()
            && self.scripts.is_empty()
            && self.modules.is_empty()
            && self.network_lexicons.is_empty()
            && self.labelers.is_empty()
    }
}

/// From the deployed snapshot and declarations to the ones a deploy would
/// install. Tests are never served, so they never appear.
pub fn between(
    old: &ProjectSnapshot,
    old_config: Option<&ProjectConfig>,
    new: &ProjectSnapshot,
    new_config: &ProjectConfig,
) -> Changes {
    let mut modules = diff_kind(&old.modules, &new.modules, same_module);
    let compiled = diff_kind(&compiled_modules(old), &compiled_modules(new), |a, b| {
        a == b
    });
    modules.added.extend(compiled.added);
    modules.changed.extend(compiled.changed);
    modules.removed.extend(compiled.removed);
    for list in [
        &mut modules.added,
        &mut modules.changed,
        &mut modules.removed,
    ] {
        list.sort();
    }

    fn network(c: &ProjectConfig) -> &Vec<String> {
        &c.network_lexicons.include
    }
    fn labels(c: &ProjectConfig) -> &Vec<String> {
        &c.labels.subscribe
    }
    fn declared(
        pick: fn(&ProjectConfig) -> &Vec<String>,
        config: Option<&ProjectConfig>,
    ) -> BTreeMap<String, ()> {
        config
            .map(|c| pick(c).iter().map(|v| (v.clone(), ())).collect())
            .unwrap_or_default()
    }

    Changes {
        lexicons: diff_kind(&old.lexicons, &new.lexicons, same_lexicon),
        scripts: diff_kind(&old.scripts, &new.scripts, same_script),
        modules,
        network_lexicons: diff_kind::<()>(
            &declared(network, old_config),
            &declared(network, Some(new_config)),
            |_, _| true,
        ),
        labelers: diff_kind::<()>(
            &declared(labels, old_config),
            &declared(labels, Some(new_config)),
            |_, _| true,
        ),
    }
}

/// Each compiled module's name and the hash of the build loaded for it.
fn compiled_modules(snapshot: &ProjectSnapshot) -> BTreeMap<String, String> {
    snapshot
        .script_modules
        .values()
        .map(|m| (m.name.clone(), m.hash.clone()))
        .collect()
}

fn diff_kind<V>(
    old: &BTreeMap<String, V>,
    new: &BTreeMap<String, V>,
    same: impl Fn(&V, &V) -> bool,
) -> KindChanges {
    let mut out = KindChanges::default();
    for (key, value) in new {
        match old.get(key) {
            None => out.added.push(key.clone()),
            Some(previous) if !same(previous, value) => out.changed.push(key.clone()),
            Some(_) => {}
        }
    }
    let kept: BTreeSet<&String> = new.keys().collect();
    out.removed = old.keys().filter(|k| !kept.contains(k)).cloned().collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, i32)]) -> BTreeMap<String, i32> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn each_key_lands_in_exactly_one_list() {
        let changes = diff_kind(
            &map(&[("kept", 1), ("edited", 1), ("dropped", 1)]),
            &map(&[("kept", 1), ("edited", 2), ("new", 1)]),
            |a, b| a == b,
        );
        assert_eq!(
            changes,
            KindChanges {
                added: vec!["new".into()],
                changed: vec!["edited".into()],
                removed: vec!["dropped".into()],
            }
        );
    }

    #[test]
    fn the_same_project_twice_changes_nothing() {
        let config: ProjectConfig = toml::from_str(
            "[network_lexicons]\ninclude = [\"app.bsky.feed.post\"]\n[labels]\nsubscribe = [\"did:plc:labeler\"]\n",
        )
        .unwrap();
        let snapshot = ProjectSnapshot::empty();
        assert!(between(&snapshot, Some(&config), &snapshot, &config).is_empty());
    }

    #[test]
    fn a_first_deploy_adds_every_declaration() {
        let config: ProjectConfig = toml::from_str(
            "[network_lexicons]\ninclude = [\"app.bsky.feed.post\"]\n[labels]\nsubscribe = [\"did:plc:labeler\"]\n",
        )
        .unwrap();
        let snapshot = ProjectSnapshot::empty();
        let changes = between(&snapshot, None, &snapshot, &config);
        assert_eq!(changes.network_lexicons.added, ["app.bsky.feed.post"]);
        assert_eq!(changes.labelers.added, ["did:plc:labeler"]);
    }

    #[test]
    fn a_dropped_declaration_is_removed() {
        let before: ProjectConfig =
            toml::from_str("[labels]\nsubscribe = [\"did:plc:labeler\"]\n").unwrap();
        let after = ProjectConfig::default();
        let snapshot = ProjectSnapshot::empty();
        let changes = between(&snapshot, Some(&before), &snapshot, &after);
        assert_eq!(changes.labelers.removed, ["did:plc:labeler"]);
        assert!(changes.labelers.added.is_empty());
    }
}
