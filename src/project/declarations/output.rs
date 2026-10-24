//! Writing one interpreter's declarations into its own directory, whole: a
//! set is built beside the old one and swapped in, so an editor never reads
//! half of one and files the interpreter no longer writes disappear.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use happyview_plugin_sdk::declarations::DeclarationFile;
use serde_json::Value;

/// HappyView's own record of what a directory was generated from.
pub const MANIFEST: &str = "manifest.json";

/// A path an interpreter returned must name a file inside its directory.
pub fn check_path(path: &str) -> Result<(), String> {
    let refused = || format!("'{path}' is not a relative path inside the output");
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return Err(refused());
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments
        .iter()
        .any(|s| s.is_empty() || *s == "." || *s == ".." || s.contains(':'))
    {
        return Err(refused());
    }
    if path == MANIFEST {
        return Err(format!(
            "'{MANIFEST}' is HappyView's record of what the directory was generated from"
        ));
    }
    Ok(())
}

pub fn language_dir(out: &Path, language_id: &str) -> PathBuf {
    out.join(language_id)
}

fn partial_dir(out: &Path, language_id: &str) -> PathBuf {
    out.join(format!(".{language_id}.partial"))
}

fn old_dir(out: &Path, language_id: &str) -> PathBuf {
    out.join(format!(".{language_id}.old"))
}

/// Remove what an interrupted write left behind.
pub fn sweep(out: &Path, language_id: &str) -> io::Result<()> {
    for dir in [partial_dir(out, language_id), old_dir(out, language_id)] {
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
    }
    Ok(())
}

/// Whether the directory was generated from exactly `manifest`.
pub fn up_to_date(out: &Path, language_id: &str, manifest: &Value) -> bool {
    fs::read(language_dir(out, language_id).join(MANIFEST))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|existing| existing == *manifest)
}

/// Write `files` and `manifest` as `<out>/<language_id>/`, replacing what was
/// there. Every path must already have passed [`check_path`].
pub fn write_language(
    out: &Path,
    language_id: &str,
    files: &[DeclarationFile],
    manifest: &Value,
) -> io::Result<()> {
    sweep(out, language_id)?;
    let partial = partial_dir(out, language_id);
    fs::create_dir_all(&partial)?;
    for file in files {
        let path = file
            .path
            .split('/')
            .fold(partial.clone(), |path, segment| path.join(segment));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, &file.contents)?;
    }
    let manifest = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
    fs::write(partial.join(MANIFEST), manifest)?;

    let target = language_dir(out, language_id);
    let old = old_dir(out, language_id);
    if target.exists() {
        fs::rename(&target, &old)?;
    }
    fs::rename(&partial, &target)?;
    if old.exists() {
        fs::remove_dir_all(&old)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paths_stay_inside() {
        for ok in [
            "a.lua",
            "happyview/db.lua",
            "lexicons/com/example/thing.d.ts",
        ] {
            assert_eq!(check_path(ok), Ok(()), "{ok}");
        }
        for bad in [
            "", "/a", "../a", "a/../b", "./a", "a//b", "a\\b", "C:/a", "c:", "a/",
        ] {
            assert_eq!(
                check_path(bad),
                Err(format!("'{bad}' is not a relative path inside the output")),
                "{bad}"
            );
        }
        assert!(
            check_path("manifest.json")
                .unwrap_err()
                .contains("HappyView's record")
        );
        assert_eq!(check_path("nested/manifest.json"), Ok(()));
    }

    #[test]
    fn a_write_replaces_the_whole_directory() {
        let out = tempfile::tempdir().unwrap();
        let file = |path: &str| DeclarationFile {
            path: path.into(),
            contents: path.into(),
        };
        let manifest = json!({"input_sha256": "a"});
        write_language(
            out.path(),
            "lua",
            &[file("a.lua"), file("x/b.lua")],
            &manifest,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(out.path().join("lua/x/b.lua")).unwrap(),
            "x/b.lua"
        );
        assert!(up_to_date(out.path(), "lua", &manifest));
        assert!(!up_to_date(
            out.path(),
            "lua",
            &json!({"input_sha256": "b"})
        ));

        fs::create_dir_all(out.path().join(".lua.partial/junk")).unwrap();
        write_language(out.path(), "lua", &[file("c.lua")], &manifest).unwrap();
        assert!(!out.path().join("lua/a.lua").exists());
        assert!(out.path().join("lua/c.lua").exists());
        assert!(!out.path().join(".lua.partial").exists());
        assert!(!out.path().join(".lua.old").exists());
    }

    #[test]
    fn a_missing_directory_is_not_up_to_date() {
        let out = tempfile::tempdir().unwrap();
        assert!(!up_to_date(out.path(), "lua", &json!({})));
    }
}
