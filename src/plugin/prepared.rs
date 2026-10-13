//! Prepared artifacts the host holds: the stamp that says which interpreter
//! build made one, and the cache that keeps a dashboard script's artifact
//! between runs.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};

use crate::plugin::{LoadedPlugin, PreparedArtifact};

/// Which interpreter build an artifact belongs to. The version alone is not
/// enough: a plugin rebuilt without a version bump may carry a different
/// engine, and loading another build's bytecode is exactly what the stamp
/// exists to prevent. Sixteen hex digits of the module's hash tell builds
/// apart without putting the whole digest in every artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactStamp {
    pub interpreter: String,
    pub version: String,
}

impl ArtifactStamp {
    pub fn of(plugin: &LoadedPlugin) -> Self {
        let digest = hex::encode(Sha256::digest(&plugin.wasm_bytes));
        Self {
            interpreter: plugin.info.id.clone(),
            version: format!("{}+{}", plugin.info.version, &digest[..16]),
        }
    }

    /// Overwrite whatever the guest wrote, so an artifact can only ever claim
    /// the build that returned it.
    pub fn apply(&self, artifact: &mut PreparedArtifact) {
        artifact.interpreter.clone_from(&self.interpreter);
        artifact.version.clone_from(&self.version);
    }

    pub fn matches(&self, artifact: &PreparedArtifact) -> bool {
        artifact.interpreter == self.interpreter && artifact.version == self.version
    }
}

/// What the cache knows about one script: the artifact, or that its
/// interpreter returned none. The second is worth remembering, because an
/// interpreter that cannot prepare would otherwise be asked again on every
/// run, doubling what each one costs.
pub type Cached = Option<Arc<PreparedArtifact>>;

/// More dashboard scripts than any instance has, and few enough that
/// starting over when it fills costs one prepare per script.
pub const PREPARED_CACHE_CAPACITY: usize = 1024;

/// Dashboard scripts' artifacts, keyed by interpreter and the SHA-256 of the
/// source. Content-keyed, so a saved edit is a miss rather than a stale hit
/// and nothing has to be told a row changed.
#[derive(Default)]
pub struct PreparedCache {
    entries: RwLock<HashMap<(String, [u8; 32]), Cached>>,
}

fn source_hash(source: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(source.as_bytes()));
    out
}

impl PreparedCache {
    /// `None` when this source was never prepared by this interpreter.
    pub fn lookup(&self, interpreter: &str, source: &str) -> Option<Cached> {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        entries
            .get(&(interpreter.to_string(), source_hash(source)))
            .cloned()
    }

    /// Store without checking which build prepared `artifact`. A caller
    /// holding a prepared result wants `PluginRegistry::store_prepared`, or
    /// `PluginExecutor::prepare_and_store`, which refuse a superseded module
    /// and stamp what they keep.
    pub fn store(
        &self,
        interpreter: &str,
        source: &str,
        artifact: Option<PreparedArtifact>,
    ) -> Cached {
        let cached = artifact.map(Arc::new);
        let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
        if entries.len() >= PREPARED_CACHE_CAPACITY {
            entries.clear();
        }
        entries.insert(
            (interpreter.to_string(), source_hash(source)),
            cached.clone(),
        );
        cached
    }

    pub fn evict_interpreter(&self, interpreter: &str) {
        let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
        entries.retain(|(id, _), _| id != interpreter);
    }

    pub fn len(&self) -> usize {
        self.entries.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(version: &str) -> PreparedArtifact {
        PreparedArtifact {
            interpreter: "lua".into(),
            version: version.into(),
            engine: "e".into(),
            format: "f".into(),
            bytes: vec![1, 2, 3],
            source_map: None,
        }
    }

    fn interpreter(version: &str, wasm: &[u8]) -> LoadedPlugin {
        let manifest: crate::plugin::PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "lua", "name": "lua", "version": version, "api_version": "2",
            "plugin_type": "interpreter", "language_id": "lua", "capabilities": [],
        }))
        .unwrap();
        LoadedPlugin {
            info: manifest.clone().into(),
            source: crate::plugin::PluginSource::File { path: "lua".into() },
            wasm_bytes: wasm.to_vec(),
            manifest: Some(manifest),
        }
    }

    /// The same version rebuilt is a different interpreter: its bytecode
    /// format may have moved, and nothing else would tell the two apart.
    #[test]
    fn a_stamp_names_the_version_and_the_build() {
        let a = ArtifactStamp::of(&interpreter("1.4.0", b"one build"));
        let b = ArtifactStamp::of(&interpreter("1.4.0", b"another build"));
        assert_eq!(a.interpreter, "lua");
        assert!(a.version.starts_with("1.4.0+"), "{}", a.version);
        assert_eq!(a.version.len(), "1.4.0+".len() + 16);
        assert_ne!(a, b);
    }

    #[test]
    fn apply_overwrites_what_the_guest_claimed_and_matches_reads_it_back() {
        let stamp = ArtifactStamp::of(&interpreter("1.4.0", b"wasm"));
        let mut forged = artifact("9.9.9");
        forged.interpreter = "someone-else".into();
        assert!(!stamp.matches(&forged));
        stamp.apply(&mut forged);
        assert!(stamp.matches(&forged));
        assert_eq!(forged.interpreter, "lua");
    }

    #[test]
    fn the_cache_tells_unknown_from_known_to_have_none() {
        let cache = PreparedCache::default();
        assert_eq!(cache.lookup("lua", "return 1"), None);
        assert_eq!(cache.store("lua", "return 1", None), None);
        assert_eq!(cache.lookup("lua", "return 1"), Some(None));

        let stored = cache.store("lua", "return 2", Some(artifact("1")));
        assert_eq!(stored.as_deref(), Some(&artifact("1")));
        assert_eq!(cache.lookup("lua", "return 2"), Some(stored));
        // Keyed by content, so an edit is a miss rather than a stale hit.
        assert_eq!(cache.lookup("lua", "return 2 "), None);
        assert_eq!(cache.lookup("js", "return 2"), None);
    }

    #[test]
    fn evicting_an_interpreter_drops_only_its_entries() {
        let cache = PreparedCache::default();
        cache.store("lua", "a", Some(artifact("1")));
        cache.store("js", "a", Some(artifact("1")));
        cache.evict_interpreter("lua");
        assert_eq!(cache.lookup("lua", "a"), None);
        assert!(cache.lookup("js", "a").is_some());
    }

    #[test]
    fn a_full_cache_starts_over_rather_than_growing() {
        let cache = PreparedCache::default();
        for i in 0..PREPARED_CACHE_CAPACITY {
            cache.store("lua", &i.to_string(), None);
        }
        assert_eq!(cache.len(), PREPARED_CACHE_CAPACITY);
        cache.store("lua", "one more", None);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.lookup("lua", "one more"), Some(None));
    }
}
