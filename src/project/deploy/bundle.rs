//! A deployed project arrives as a tar.gz of the files `happyview check`
//! would read, and nothing else. Unpacking it is where a hostile archive is
//! stopped: every entry is checked before a byte of it reaches the disk.

use std::path::Path;

use crate::project::Diagnostic;

/// Largest request body accepted, compressed.
pub const MAX_COMPRESSED_BYTES: usize = 50 * 1024 * 1024;
/// Largest total of the archive's file contents once decompressed, so a
/// small archive cannot expand to fill the disk.
pub const MAX_EXPANDED_BYTES: u64 = 200 * 1024 * 1024;
/// Most entries an archive may hold, directories included.
pub const MAX_ENTRIES: usize = 10_000;

/// Unpack `archive` into `dest`, which must exist and be empty. Answers every
/// problem found rather than the first, so one deploy reports them all; on
/// any problem `dest` holds a partial tree the caller discards.
pub fn unpack(archive: &[u8], dest: &Path) -> Result<(), Vec<Diagnostic>> {
    let mut problems = Vec::new();
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let entries = match tar.entries() {
        Ok(entries) => entries,
        Err(e) => return Err(vec![unreadable(e)]),
    };
    let mut seen = std::collections::HashSet::new();
    let mut expanded: u64 = 0;
    for (count, entry) in entries.enumerate() {
        if count == MAX_ENTRIES {
            problems.push(Diagnostic::error(
                "",
                format!("the bundle holds more than {MAX_ENTRIES} entries"),
            ));
            break;
        }
        let mut entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                problems.push(unreadable(e));
                break;
            }
        };
        let raw = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        let path = match normalize(&raw) {
            Ok(path) => path,
            Err(why) => {
                problems.push(Diagnostic::error(raw, why));
                continue;
            }
        };
        if kind != tar::EntryType::Regular {
            problems.push(Diagnostic::error(
                path,
                "only regular files can be deployed; links and devices are refused",
            ));
            continue;
        }
        if !allowed(&path) {
            problems.push(Diagnostic::error(
                path,
                "is not part of a project; a bundle holds happyview.toml, src/ and .happyview/build/<module>.wasm",
            ));
            continue;
        }
        if !seen.insert(path.clone()) {
            problems.push(Diagnostic::error(path, "appears in the bundle twice"));
            continue;
        }
        let size = entry.header().size().unwrap_or(u64::MAX);
        expanded = expanded.saturating_add(size);
        if expanded > MAX_EXPANDED_BYTES {
            problems.push(Diagnostic::error(
                "",
                format!(
                    "the bundle expands past {} MiB",
                    MAX_EXPANDED_BYTES / (1024 * 1024)
                ),
            ));
            break;
        }
        if let Err(e) = write(&mut entry, &dest.join(&path), size) {
            problems.push(Diagnostic::error(
                path,
                format!("could not be unpacked: {e}"),
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn unreadable(e: std::io::Error) -> Diagnostic {
    Diagnostic::error("", format!("the bundle is not a readable tar.gz: {e}"))
}

/// The entry's path relative to the project root, or why it has none. A
/// leading `./` is how most tools write an archive and means nothing.
fn normalize(raw: &str) -> Result<String, &'static str> {
    let mut path = raw;
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    if path.starts_with('/') {
        return Err("is an absolute path; bundle paths are relative to the project root");
    }
    if path.contains('\\') {
        return Err("contains a backslash; bundle paths use /");
    }
    if path.is_empty()
        || path
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err("is not a plain relative path inside the project");
    }
    Ok(path.to_string())
}

fn allowed(path: &str) -> bool {
    if path == crate::config::project::PROJECT_FILE {
        return true;
    }
    if path.starts_with("src/") {
        return true;
    }
    path.strip_prefix(".happyview/build/")
        .and_then(|name| name.strip_suffix(".wasm"))
        .is_some_and(|stem| !stem.is_empty() && !stem.contains('/'))
}

fn write(entry: &mut impl std::io::Read, target: &Path, size: u64) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(target)?;
    let written = std::io::copy(&mut std::io::Read::take(entry, size), &mut file)?;
    if written != size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "the archive ended inside this file",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;

    enum Entry<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        Symlink(&'a str, &'a str),
    }

    /// An archive built entry by entry, with paths written raw so a test can
    /// produce what `tar::Builder` itself would refuse to.
    fn archive(entries: &[Entry<'_>]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for entry in entries {
            let mut header = tar::Header::new_gnu();
            match entry {
                Entry::File(path, contents) => {
                    set_raw_path(&mut header, path);
                    header.set_size(contents.len() as u64);
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_mode(0o644);
                    header.set_cksum();
                    builder.append(&header, *contents).unwrap();
                }
                Entry::Dir(path) => {
                    set_raw_path(&mut header, path);
                    header.set_size(0);
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_mode(0o755);
                    header.set_cksum();
                    builder.append(&header, std::io::empty()).unwrap();
                }
                Entry::Symlink(path, target) => {
                    set_raw_path(&mut header, path);
                    header.set_size(0);
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_link_name(target).unwrap();
                    header.set_mode(0o777);
                    header.set_cksum();
                    builder.append(&header, std::io::empty()).unwrap();
                }
            }
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn set_raw_path(header: &mut tar::Header, path: &str) {
        let name = &mut header.as_old_mut().name;
        name.fill(0);
        name[..path.len()].copy_from_slice(path.as_bytes());
    }

    fn unpacked(entries: &[Entry<'_>]) -> (tempfile::TempDir, Result<(), Vec<Diagnostic>>) {
        let dir = tempfile::tempdir().unwrap();
        let result = unpack(&archive(entries), dir.path());
        (dir, result)
    }

    fn problems(entries: &[Entry<'_>]) -> Vec<String> {
        unpacked(entries)
            .1
            .expect_err("the archive was accepted")
            .into_iter()
            .map(|d| d.to_string())
            .collect()
    }

    #[test]
    fn a_project_unpacks_to_the_files_it_holds() {
        let (dir, result) = unpacked(&[
            Entry::File("happyview.toml", b"[lexicons]\n"),
            Entry::Dir("src/"),
            Entry::File("src/lexicons/com/example/thing.json", b"{}"),
            Entry::File("./src/thing.lua", b"return 1"),
            Entry::File(".happyview/build/format.wasm", b"\0asm"),
        ]);
        result.unwrap();
        let read = |p: &str| std::fs::read(dir.path().join(p)).unwrap();
        assert_eq!(read("happyview.toml"), b"[lexicons]\n");
        assert_eq!(read("src/lexicons/com/example/thing.json"), b"{}");
        assert_eq!(read("src/thing.lua"), b"return 1");
        assert_eq!(read(".happyview/build/format.wasm"), b"\0asm");
    }

    #[test]
    fn anything_outside_the_allowlist_is_refused_by_name() {
        let found = problems(&[
            Entry::File(".env", b"SECRET=1"),
            Entry::File(".happyview/plugins/lua/manifest.json", b"{}"),
            Entry::File(".happyview/build/nested/x.wasm", b""),
            Entry::File(".happyview/build/notes.txt", b""),
            Entry::File("README.md", b""),
            Entry::File("src/ok.lua", b""),
        ]);
        assert_eq!(found.len(), 5, "{found:?}");
        for path in [
            ".env",
            ".happyview/plugins/lua/manifest.json",
            ".happyview/build/nested/x.wasm",
            ".happyview/build/notes.txt",
            "README.md",
        ] {
            assert!(
                found.iter().any(|p| p.starts_with(&format!("{path}: "))),
                "{path} not named in {found:?}"
            );
        }
    }

    #[test]
    fn an_entry_that_escapes_the_root_never_reaches_the_disk() {
        let outside = tempfile::tempdir().unwrap();
        let escape = format!("{}/escaped", outside.path().display());
        let found = problems(&[
            Entry::File("src/../../escaped", b"x"),
            Entry::File(&escape, b"x"),
            Entry::Symlink("src/link.lua", "/etc/passwd"),
        ]);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(!outside.path().join("escaped").exists());
    }

    #[test]
    fn a_path_given_twice_is_refused() {
        let found = problems(&[
            Entry::File("src/a.lua", b"one"),
            Entry::File("./src/a.lua", b"two"),
        ]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("src/a.lua: "), "{found:?}");
    }

    #[test]
    fn an_archive_that_expands_past_the_limit_is_refused() {
        // Zeros compress to almost nothing, which is the attack.
        let big = vec![0u8; (MAX_EXPANDED_BYTES + 1) as usize];
        let found = problems(&[Entry::File("src/big.json", &big)]);
        assert!(
            found.iter().any(|p| p.contains("expands past")),
            "{found:?}"
        );
    }

    #[test]
    fn an_archive_with_too_many_entries_is_refused() {
        let names: Vec<String> = (0..=MAX_ENTRIES).map(|i| format!("src/{i}.lua")).collect();
        let entries: Vec<Entry<'_>> = names.iter().map(|n| Entry::File(n, b"")).collect();
        let found = problems(&entries);
        assert!(found.iter().any(|p| p.contains("entries")), "{found:?}");
    }

    #[test]
    fn a_body_that_is_not_a_gzipped_tar_is_one_problem() {
        let dir = tempfile::tempdir().unwrap();
        let found = unpack(b"not an archive", dir.path()).unwrap_err();
        assert_eq!(found.len(), 1, "{found:?}");
    }
}
