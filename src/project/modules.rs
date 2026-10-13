//! Resolving a relative import to a module in the project snapshot.
//!
//! Pure over a snapshot: the run pins one when it starts, and resolving
//! against that rather than the live one is what stops a rebuild mid-run
//! mixing an old handler with a new module.

use crate::catalog::path_string;
use crate::project::snapshot::{ProjectModule, ProjectSnapshot};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModuleError {
    #[error(
        "'{specifier}' is not a relative path: a local module is named with './' or '../', and a bare name is a library"
    )]
    NotRelative { specifier: String },
    #[error("'{specifier}' uses '\\'; module paths are separated with '/' on every platform")]
    Malformed { specifier: String },
    #[error(
        "'{referrer}' is not a file under src/, so '{specifier}' has nothing to be relative to"
    )]
    BadReferrer { specifier: String, referrer: String },
    #[error(
        "'{specifier}' from {referrer} climbs out of src/, and local modules can only come from inside it"
    )]
    OutsideSrc { specifier: String, referrer: String },
    #[error("{0}")]
    NotFound(String),
}

pub fn is_relative(specifier: &str) -> bool {
    specifier.starts_with("./") || specifier.starts_with("../")
}

/// Normalise `specifier` against the directory `referrer` sits in, keeping
/// the result inside `src/`.
fn join(specifier: &str, referrer: &str) -> Result<String, ModuleError> {
    // Before the prefix check: `.\lib\format` was meant as relative, and
    // saying why it fails beats calling it a library name.
    if specifier.contains('\\') {
        return Err(ModuleError::Malformed {
            specifier: specifier.to_string(),
        });
    }
    if !is_relative(specifier) {
        return Err(ModuleError::NotRelative {
            specifier: specifier.to_string(),
        });
    }
    let mut parts: Vec<&str> = referrer.split('/').collect();
    if parts.len() < 2 || parts[0] != "src" {
        return Err(ModuleError::BadReferrer {
            specifier: specifier.to_string(),
            referrer: referrer.to_string(),
        });
    }
    // The referrer's own file name: what follows is relative to its directory.
    parts.pop();
    for segment in specifier.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if parts.len() <= 1 {
                    return Err(ModuleError::OutsideSrc {
                        specifier: specifier.to_string(),
                        referrer: referrer.to_string(),
                    });
                }
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

/// The files `base` may name, in the order they are tried: itself when it
/// already carries one of the caller's extensions, then each extension
/// appended.
fn candidates(base: &str, extensions: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let file = base.rsplit('/').next().unwrap_or(base);
    if let Some((_, ext)) = file.rsplit_once('.')
        && extensions.iter().any(|e| e == ext)
    {
        out.push(base.to_string());
    }
    out.extend(extensions.iter().map(|ext| format!("{base}.{ext}")));
    out
}

fn strip_extension(path: &str) -> &str {
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    match file.rsplit_once('.') {
        Some((stem, _)) => &path[..dir.len() + usize::from(!dir.is_empty()) + stem.len()],
        None => path,
    }
}

pub fn resolve_module<'s>(
    snapshot: &'s ProjectSnapshot,
    specifier: &str,
    referrer: &str,
    extensions: &[String],
    language: &str,
) -> Result<&'s ProjectModule, ModuleError> {
    let base = join(specifier, referrer)?;
    let tried = candidates(&base, extensions);
    // The extensions are the caller's today, but a file was claimed by the
    // build that made this snapshot. If an interpreter has since changed what
    // it claims and the rebuild failed, the old snapshot keeps serving, and a
    // path match alone would hand one language's source to another.
    let found = tried.iter().find_map(|path| snapshot.modules.get(path));
    if let Some(module) = found
        && module.language_id == language
    {
        return Ok(module);
    }
    Err(ModuleError::NotFound(not_found(
        snapshot, specifier, referrer, &base, &tried, extensions, language,
    )))
}

fn not_found(
    snapshot: &ProjectSnapshot,
    specifier: &str,
    referrer: &str,
    base: &str,
    tried: &[String],
    extensions: &[String],
    language: &str,
) -> String {
    let looked = tried.join(", ");
    let mut message = format!("no module '{specifier}' from {referrer}: looked for {looked}");

    if let Some(other) = tried
        .iter()
        .find_map(|path| snapshot.modules.get(path))
        .filter(|module| module.language_id != language)
    {
        message.push_str(&format!(
            "; {} exists, but it is a {} module, and {language} scripts can only import {language} files",
            other.id, other.language_id
        ));
        return message;
    }

    if let Some(handler) = snapshot
        .scripts
        .values()
        .find(|script| tried.contains(&path_string(&script.path)))
    {
        message.push_str(&format!(
            "; {} is the handler for {}, and a handler cannot be imported — move the shared code into a file of its own",
            path_string(&handler.path),
            handler.trigger
        ));
        return message;
    }

    let stem = strip_extension(base);
    let wanted: Vec<&str> = extensions.iter().map(String::as_str).collect();
    let elsewhere = snapshot
        .modules
        .keys()
        .cloned()
        .chain(snapshot.scripts.values().map(|s| path_string(&s.path)))
        .find(|path| {
            strip_extension(path) == stem
                && path
                    .rsplit_once('.')
                    .is_some_and(|(_, ext)| !wanted.contains(&ext))
        });
    if let Some(other) = elsewhere {
        let exts = wanted
            .iter()
            .map(|e| format!(".{e}"))
            .collect::<Vec<_>>()
            .join(", ");
        message.push_str(&format!(
            "; {other} exists, but {language} scripts can only import {exts} files"
        ));
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::snapshot::{ProjectModule, ProjectScript, ProjectSnapshot, ScriptBody};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn lua() -> Vec<String> {
        vec!["lua".to_string()]
    }

    fn module(path: &str, language: &str) -> ProjectModule {
        ProjectModule {
            id: path.to_string(),
            path: PathBuf::from(path),
            language_id: language.to_string(),
            source: Arc::from("return {}"),
            artifact: None,
        }
    }

    fn snapshot(modules: &[(&str, &str)]) -> ProjectSnapshot {
        let mut snap = ProjectSnapshot::empty();
        for (path, language) in modules {
            snap.modules
                .insert(path.to_string(), module(path, language));
        }
        snap
    }

    fn id_of(result: Result<&ProjectModule, ModuleError>) -> String {
        result
            .map(|m| m.id.clone())
            .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn a_sibling_and_a_parent_resolve_with_the_callers_extension() {
        let snap = snapshot(&[("src/lib/format.lua", "lua"), ("src/jobs/util.lua", "lua")]);
        assert_eq!(
            id_of(resolve_module(
                &snap,
                "../lib/format",
                "src/jobs/tick.lua",
                &lua(),
                "lua"
            )),
            "src/lib/format.lua"
        );
        assert_eq!(
            id_of(resolve_module(
                &snap,
                "./util",
                "src/jobs/tick.lua",
                &lua(),
                "lua"
            )),
            "src/jobs/util.lua"
        );
        // Naming the extension is the same file, so the same id.
        assert_eq!(
            id_of(resolve_module(
                &snap,
                "./util.lua",
                "src/jobs/tick.lua",
                &lua(),
                "lua"
            )),
            "src/jobs/util.lua"
        );
    }

    #[test]
    fn a_module_another_language_claimed_is_not_resolved() {
        let snap = snapshot(&[("src/lib/format.lua", "teal")]);
        let err =
            resolve_module(&snap, "../lib/format", "src/jobs/tick.lua", &lua(), "lua").unwrap_err();
        let ModuleError::NotFound(message) = &err else {
            panic!("expected not found, got {err}");
        };
        assert!(
            message.ends_with(
                "; src/lib/format.lua exists, but it is a teal module, and lua scripts can only import lua files"
            ),
            "{message}"
        );
    }

    #[test]
    fn a_path_that_normalises_back_inside_src_is_the_same_module() {
        let snap = snapshot(&[("src/lib/format.lua", "lua")]);
        for specifier in [
            "./../lib/./format",
            "../lib/../lib/format",
            "./a/../../lib/format",
        ] {
            assert_eq!(
                id_of(resolve_module(
                    &snap,
                    specifier,
                    "src/jobs/tick.lua",
                    &lua(),
                    "lua"
                )),
                "src/lib/format.lua",
                "{specifier}"
            );
        }
    }

    #[test]
    fn climbing_out_of_src_is_refused_however_it_is_spelt() {
        let snap = snapshot(&[]);
        for specifier in [
            "../../happyview.toml",
            "../../../etc/passwd",
            "./../../.env",
        ] {
            let err =
                resolve_module(&snap, specifier, "src/jobs/tick.lua", &lua(), "lua").unwrap_err();
            assert!(
                matches!(err, ModuleError::OutsideSrc { .. }),
                "{specifier}: {err}"
            );
            assert!(err.to_string().contains("src/"), "{err}");
        }
    }

    #[test]
    fn a_bare_or_absolute_specifier_is_not_a_local_module() {
        let snap = snapshot(&[]);
        for specifier in ["happyview.record", "lib/format", "/src/lib/format", ""] {
            assert!(!is_relative(specifier), "{specifier}");
            let err =
                resolve_module(&snap, specifier, "src/jobs/tick.lua", &lua(), "lua").unwrap_err();
            assert!(
                matches!(err, ModuleError::NotRelative { .. }),
                "{specifier}: {err}"
            );
        }
        assert!(is_relative("./x") && is_relative("../x"));
    }

    #[test]
    fn a_backslash_is_refused_rather_than_read_as_a_separator() {
        let snap = snapshot(&[("src/lib/format.lua", "lua")]);
        let err = resolve_module(&snap, ".\\lib\\format", "src/x.lua", &lua(), "lua").unwrap_err();
        assert!(matches!(err, ModuleError::Malformed { .. }), "{err}");
        assert!(err.to_string().contains('/'), "{err}");
    }

    #[test]
    fn a_referrer_outside_src_has_nothing_to_be_relative_to() {
        let snap = snapshot(&[]);
        for referrer in ["script", "lib/format.lua", "/src/x.lua"] {
            let err = resolve_module(&snap, "./x", referrer, &lua(), "lua").unwrap_err();
            assert!(
                matches!(err, ModuleError::BadReferrer { .. }),
                "{referrer}: {err}"
            );
        }
    }

    #[test]
    fn a_miss_names_the_specifier_the_importer_and_where_it_looked() {
        let snap = snapshot(&[]);
        let err =
            resolve_module(&snap, "./missing", "src/jobs/tick.lua", &lua(), "lua").unwrap_err();
        let ModuleError::NotFound(message) = &err else {
            panic!("expected not found, got {err}");
        };
        assert!(message.contains("./missing"), "{message}");
        assert!(message.contains("src/jobs/tick.lua"), "{message}");
        assert!(message.contains("src/jobs/missing.lua"), "{message}");
    }

    #[test]
    fn a_miss_names_a_same_named_file_in_another_language() {
        let snap = snapshot(&[("src/lib/format.ts", "typescript")]);
        let err =
            resolve_module(&snap, "../lib/format", "src/jobs/tick.lua", &lua(), "lua").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("src/lib/format.ts"), "{message}");
        assert!(message.contains("lua"), "{message}");
    }

    #[test]
    fn a_handler_cannot_be_imported_and_the_miss_says_so() {
        let mut snap = snapshot(&[]);
        snap.scripts.insert(
            "xrpc.query:com.example.getThing".to_string(),
            ProjectScript {
                trigger: "xrpc.query:com.example.getThing".to_string(),
                path: PathBuf::from("src/lexicons/com/example/getThing.lua"),
                language_id: "lua".to_string(),
                body: ScriptBody::Source {
                    source: Arc::from("function handle() end"),
                    artifact: None,
                },
                description: None,
                outbound_xrpcs: Vec::new(),
            },
        );
        let err = resolve_module(
            &snap,
            "./getThing",
            "src/lexicons/com/example/thing.index.lua",
            &lua(),
            "lua",
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("xrpc.query:com.example.getThing"),
            "{message}"
        );
        assert!(message.contains("handler"), "{message}");
    }
}
