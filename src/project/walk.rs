//! The files under a project's `src/`, in a stable order.

use std::path::{Path, PathBuf};

use super::diagnostic::Diagnostic;

/// A regular file under `src/`.
#[derive(Debug, Clone)]
pub struct SrcFile {
    /// Path segments under `src/`, e.g. `["jobs", "nightly.lua"]`.
    pub segments: Vec<String>,
    /// Where the file is on disk, for reading it.
    pub abs: PathBuf,
}

impl SrcFile {
    /// Relative to the project root: `src/jobs/nightly.lua`.
    pub fn rel_path(&self) -> PathBuf {
        rel_path(&self.segments)
    }

    /// `rel_path` as a string. A module's id, a chunk's name and a
    /// resolver's referrer are all this one string.
    pub fn id(&self) -> String {
        format!("src/{}", self.segments.join("/"))
    }

    pub fn name(&self) -> &str {
        self.segments.last().map(String::as_str).unwrap_or_default()
    }

    /// The directories between `src/` and the file.
    pub fn dir(&self) -> &[String] {
        &self.segments[..self.segments.len().saturating_sub(1)]
    }

    /// The name up to its last dot.
    pub fn stem(&self) -> &str {
        let name = self.name();
        name.rsplit_once('.').map_or(name, |(stem, _)| stem)
    }

    /// The file as text. A byte-order mark is dropped, because JSON parsers
    /// refuse one and editors on some platforms add one unasked.
    pub fn read_text(&self) -> Result<String, Diagnostic> {
        let bytes = std::fs::read(&self.abs).map_err(|e| {
            Diagnostic::error(self.rel_path(), format!("could not read the file: {e}"))
        })?;
        let text = String::from_utf8(bytes)
            .map_err(|_| Diagnostic::error(self.rel_path(), "the file is not UTF-8 text"))?;
        Ok(match text.strip_prefix('\u{feff}') {
            Some(rest) => rest.to_string(),
            None => text,
        })
    }
}

fn rel_path(segments: &[String]) -> PathBuf {
    let mut path = String::from("src");
    for segment in segments {
        path.push('/');
        path.push_str(segment);
    }
    PathBuf::from(path)
}

/// Names that are never part of a project: dotfiles and directories, and the
/// swap and backup files editors leave beside the file being edited.
fn is_skipped(name: &str) -> bool {
    name.starts_with('.') || name.starts_with('#') || name.ends_with('~')
}

/// Every file under `<root>/src`, sorted by path.
pub fn walk_src(root: &Path) -> (Vec<SrcFile>, Vec<Diagnostic>) {
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    let src = root.join("src");
    if !src.is_dir() {
        diagnostics.push(Diagnostic::error(
            "src",
            format!("{} has no src/ directory", root.display()),
        ));
        return (files, diagnostics);
    }
    walk_dir(&src, &mut Vec::new(), &mut files, &mut diagnostics);
    (files, diagnostics)
}

/// The entries that could be read. One that could not is an error, because a
/// build is all-or-nothing and a file it could not see must not read as a
/// file that is absent.
fn readable_entries<T>(
    entries: impl IntoIterator<Item = std::io::Result<T>>,
    segments: &[String],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<T> {
    let mut readable = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => readable.push(entry),
            Err(e) => diagnostics.push(Diagnostic::error(
                rel_path(segments),
                format!("could not read an entry of the directory: {e}"),
            )),
        }
    }
    readable
}

fn walk_dir(
    dir: &Path,
    segments: &mut Vec<String>,
    files: &mut Vec<SrcFile>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            diagnostics.push(Diagnostic::error(
                rel_path(segments),
                format!("could not read the directory: {e}"),
            ));
            return;
        }
    };
    let mut entries = readable_entries(entries, segments, diagnostics);
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            diagnostics.push(Diagnostic::error(
                rel_path(segments),
                format!("{:?} is not a UTF-8 file name", entry.file_name()),
            ));
            continue;
        };
        if is_skipped(&name) {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(e) => {
                segments.push(name);
                diagnostics.push(Diagnostic::error(
                    rel_path(segments),
                    format!("could not read the file type: {e}"),
                ));
                segments.pop();
                continue;
            }
        };
        let path = entry.path();
        segments.push(name);
        if file_type.is_symlink() {
            // A linked directory could lead out of the project or back into
            // itself; a linked file is just a file.
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => files.push(SrcFile {
                    segments: segments.clone(),
                    abs: path,
                }),
                Ok(_) => diagnostics.push(Diagnostic::warning(
                    rel_path(segments),
                    "symlinked directories are not followed",
                )),
                Err(e) => diagnostics.push(Diagnostic::error(
                    rel_path(segments),
                    format!("broken symlink: {e}"),
                )),
            }
        } else if file_type.is_dir() {
            walk_dir(&path, segments, files, diagnostics);
        } else if file_type.is_file() {
            files.push(SrcFile {
                segments: segments.clone(),
                abs: path,
            });
        }
        segments.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn ids(files: &[SrcFile]) -> Vec<String> {
        files.iter().map(SrcFile::id).collect()
    }

    #[test]
    fn an_entry_that_cannot_be_read_is_an_error() {
        let mut diagnostics = Vec::new();
        let entries = vec![Ok(1), Err(std::io::Error::other("boom")), Ok(2)];
        let segments = vec!["jobs".to_string()];
        let readable = readable_entries(entries, &segments, &mut diagnostics);
        assert_eq!(readable, [1, 2]);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].path, PathBuf::from("src/jobs"));
        assert!(diagnostics[0].message.contains("boom"));
    }

    #[test]
    fn every_file_under_src_is_found_in_path_order() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "src/lexicons/com/example/getThing.json", "{}");
        touch(dir.path(), "src/lexicons/com/example/getThing.lua", "");
        touch(dir.path(), "src/jobs/nightly.lua", "");
        touch(dir.path(), "happyview.toml", "");

        let (files, diagnostics) = walk_src(dir.path());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(
            ids(&files),
            [
                "src/jobs/nightly.lua",
                "src/lexicons/com/example/getThing.json",
                "src/lexicons/com/example/getThing.lua",
            ]
        );
        let file = &files[1];
        assert_eq!(
            file.rel_path(),
            PathBuf::from("src/lexicons/com/example/getThing.json")
        );
        assert_eq!(file.name(), "getThing.json");
        assert_eq!(file.stem(), "getThing");
        assert_eq!(file.dir(), ["lexicons", "com", "example"]);
    }

    /// Saving in vim or emacs leaves these beside the real file for as long
    /// as the editor is open; a build that read them would fail on every save.
    #[test]
    fn dotfiles_and_editor_leftovers_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "src/lib/format.lua", "");
        touch(dir.path(), "src/lib/format.lua~", "");
        touch(dir.path(), "src/lib/#format.lua#", "");
        touch(dir.path(), "src/lib/.format.lua.swp", "");
        touch(dir.path(), "src/.DS_Store", "");
        touch(dir.path(), "src/.git/config", "");

        let (files, diagnostics) = walk_src(dir.path());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(ids(&files), ["src/lib/format.lua"]);
    }

    #[test]
    fn a_missing_src_is_one_error() {
        let dir = tempfile::tempdir().unwrap();
        let (files, diagnostics) = walk_src(dir.path());
        assert!(files.is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].is_error());
        assert_eq!(diagnostics[0].path, PathBuf::from("src"));
        assert!(
            diagnostics[0].message.contains("no src/ directory"),
            "{}",
            diagnostics[0]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_files_are_read_and_symlinked_directories_are_not() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "shared/format.lua", "return {}");
        touch(dir.path(), "src/lib/keep.lua", "");
        fs::create_dir_all(dir.path().join("src/lib")).unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("shared/format.lua"),
            dir.path().join("src/lib/format.lua"),
        )
        .unwrap();
        std::os::unix::fs::symlink(dir.path().join("shared"), dir.path().join("src/shared"))
            .unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("nowhere.lua"),
            dir.path().join("src/lib/gone.lua"),
        )
        .unwrap();

        let (files, diagnostics) = walk_src(dir.path());
        assert_eq!(ids(&files), ["src/lib/format.lua", "src/lib/keep.lua"]);
        let shown: Vec<String> = diagnostics.iter().map(ToString::to_string).collect();
        assert_eq!(shown.len(), 2, "{shown:?}");
        assert!(
            shown
                .iter()
                .any(|d| d.starts_with("src/lib/gone.lua: ") && d.contains("broken symlink")),
            "{shown:?}"
        );
        assert!(
            shown.iter().any(|d| d.starts_with("src/shared: warning: ")),
            "{shown:?}"
        );
    }

    #[test]
    fn read_text_strips_a_byte_order_mark_and_refuses_binary() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "src/a.lua", "\u{feff}return 1");
        fs::write(dir.path().join("src/b.lua"), [0xff, 0xfe, 0x00]).unwrap();
        let (files, _) = walk_src(dir.path());
        assert_eq!(files[0].read_text().unwrap(), "return 1");
        let err = files[1].read_text().unwrap_err();
        assert_eq!(err.to_string(), "src/b.lua: the file is not UTF-8 text");
    }
}
