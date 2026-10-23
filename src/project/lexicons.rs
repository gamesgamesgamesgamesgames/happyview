//! A lexicon file in its snapshot form, with its `happyview.toml` overrides
//! applied.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::config::project::{LexiconOverride, PROJECT_FILE};
use crate::lexicon::{ParsedLexicon, ProcedureAction};

use super::diagnostic::Diagnostic;
use super::snapshot::ProjectLexicon;
use super::walk::SrcFile;

/// The NSID a lexicon file's path names: its directories under
/// `src/lexicons/` and its stem, joined with dots.
pub fn expected_id(file: &SrcFile) -> String {
    let mut parts: Vec<&str> = file.dir().iter().skip(1).map(String::as_str).collect();
    parts.push(file.stem());
    parts.join(".")
}

pub fn load_lexicon(
    file: &SrcFile,
    overrides: &BTreeMap<String, LexiconOverride>,
) -> Result<ProjectLexicon, Diagnostic> {
    let path = file.rel_path();
    let text = file.read_text()?;
    let raw: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        Diagnostic::error(&path, format!("invalid JSON: {e}")).at_line(u32::try_from(e.line()).ok())
    })?;
    let id = crate::lexicon::check_document(&raw).map_err(|e| Diagnostic::error(&path, e))?;

    // The id is what clients call and what the catalog keys on, so it wins;
    // the path is only a convention, but it is the one a reader trusts.
    let expected = expected_id(file);
    if id != expected {
        return Err(Diagnostic::error(
            &path,
            format!(
                "the lexicon's id is '{id}' but its path names '{expected}'; move the file to src/lexicons/{}.json or change the id",
                id.replace('.', "/")
            ),
        ));
    }

    let overrides = overrides.get(&id);
    let action = overrides
        .and_then(|o| o.action)
        .map(ProcedureAction::from)
        .unwrap_or(ProcedureAction::Upsert);
    let parsed = ParsedLexicon::parse(
        raw,
        1,
        overrides.and_then(|o| o.target_collection.clone()),
        action,
        overrides.and_then(|o| o.token_cost),
    )
    .map_err(|e| Diagnostic::error(&path, format!("failed to parse lexicon: {e}")))?;
    Ok(project_lexicon(
        parsed,
        path,
        overrides.and_then(|o| o.backfill).unwrap_or(false),
    ))
}

/// The one place a `ProjectLexicon` is built.
fn project_lexicon(parsed: ParsedLexicon, path: PathBuf, backfill: bool) -> ProjectLexicon {
    ProjectLexicon {
        parsed,
        path,
        backfill,
        indexed: true,
    }
}

/// `[lexicons."…"]` keys naming no lexicon file. A stale override is most
/// likely a typo or a rename, and ignoring one is how `backfill` ends up off
/// in production.
pub fn stale_overrides(
    overrides: &BTreeMap<String, LexiconOverride>,
    lexicon_files: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    overrides
        .keys()
        .filter(|id| !lexicon_files.contains(*id))
        .map(|id| {
            Diagnostic::error(
                PROJECT_FILE,
                format!(
                    "[lexicons.\"{id}\"] names no lexicon under src/lexicons/; \
                     a stale override is most likely a typo or a rename"
                ),
            )
        })
        .collect()
}

/// Two lexicon files whose paths name the same NSID (`com/example.foo.json`
/// and `com/example/foo.json`). Each later file is an error naming the
/// first, because keeping whichever is read last would make the build depend
/// on directory order.
pub fn duplicate_ids(files: &[&SrcFile]) -> Vec<Diagnostic> {
    let mut first: BTreeMap<String, (&SrcFile, String)> = BTreeMap::new();
    let mut found = Vec::new();
    for file in files {
        let id = expected_id(file);
        // atproto forbids two NSIDs that differ only by casing, so the
        // comparison folds case even though the id-versus-path one does not.
        match first.get(&id.to_ascii_lowercase()) {
            Some((earlier, earlier_id)) => {
                let message = if *earlier_id == id {
                    format!(
                        "{} also names the lexicon '{id}'; keep one of the two files",
                        earlier.rel_path().display()
                    )
                } else {
                    format!(
                        "{} names '{earlier_id}', which differs from '{id}' only by case; \
                         atproto allows only one of them",
                        earlier.rel_path().display()
                    )
                };
                found.push(Diagnostic::error(file.rel_path(), message));
            }
            None => {
                first.insert(id.to_ascii_lowercase(), (file, id));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::project::{LexiconOverride, OverrideAction};
    use crate::lexicon::{LexiconType, ProcedureAction};
    use serde_json::json;

    fn write(dir: &tempfile::TempDir, rel: &str, contents: &str) -> SrcFile {
        let abs = dir.path().join("src").join(rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, contents).unwrap();
        SrcFile {
            segments: rel.split('/').map(str::to_string).collect(),
            abs,
        }
    }

    fn query(id: &str) -> String {
        json!({"lexicon": 1, "id": id, "defs": {"main": {"type": "query"}}}).to_string()
    }

    #[test]
    fn a_lexicon_loads_with_its_overrides_applied() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "lexicons/com/example/getThing.json",
            &query("com.example.getThing"),
        );
        let overrides = BTreeMap::from([(
            "com.example.getThing".to_string(),
            LexiconOverride {
                target_collection: Some("com.example.thing".into()),
                token_cost: Some(5),
                backfill: Some(true),
                ..Default::default()
            },
        )]);
        let lexicon = load_lexicon(&file, &overrides).unwrap();
        assert_eq!(lexicon.parsed.id, "com.example.getThing");
        assert_eq!(lexicon.parsed.lexicon_type, LexiconType::Query);
        assert_eq!(
            lexicon.parsed.target_collection.as_deref(),
            Some("com.example.thing")
        );
        assert_eq!(lexicon.parsed.token_cost, Some(5));
        assert!(lexicon.backfill);
        assert_eq!(
            lexicon.path,
            PathBuf::from("src/lexicons/com/example/getThing.json")
        );

        let plain = load_lexicon(&file, &BTreeMap::new()).unwrap();
        assert!(!plain.backfill);
        assert!(plain.parsed.target_collection.is_none());
    }

    #[test]
    fn a_path_that_disagrees_with_the_id_names_both() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "lexicons/com/example/getThing.json",
            &query("com.example.getOther"),
        );
        let err = load_lexicon(&file, &BTreeMap::new()).unwrap_err();
        assert_eq!(
            err.path,
            PathBuf::from("src/lexicons/com/example/getThing.json")
        );
        assert!(err.message.contains("'com.example.getOther'"), "{err}");
        assert!(err.message.contains("'com.example.getThing'"), "{err}");
        assert!(
            err.message
                .contains("src/lexicons/com/example/getOther.json"),
            "{err}"
        );
    }

    /// On a case-insensitive filesystem the file opens under either name,
    /// which is exactly why the path can't be trusted to match.
    #[test]
    fn a_path_differing_only_by_case_is_still_a_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "lexicons/com/example/getthing.json",
            &query("com.example.getThing"),
        );
        let err = load_lexicon(&file, &BTreeMap::new()).unwrap_err();
        assert!(err.message.contains("'com.example.getthing'"), "{err}");
        assert!(err.message.contains("'com.example.getThing'"), "{err}");
    }

    #[test]
    fn ids_differing_only_by_case_are_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let upper = write(&dir, "lexicons/com/Example/thing.json", "{}");
        let lower = write(&dir, "lexicons/com/example/thing.json", "{}");
        let found = duplicate_ids(&[&upper, &lower]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].path,
            PathBuf::from("src/lexicons/com/example/thing.json")
        );
        assert!(found[0].message.contains("only by case"), "{:?}", found[0]);
        assert!(found[0].message.contains("'com.Example.thing'"));
        assert!(found[0].message.contains("'com.example.thing'"));
    }

    #[test]
    fn invalid_json_reports_its_line() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "lexicons/com/example/getThing.json",
            "{\n  \"lexicon\": 1,\n  oops\n}",
        );
        let err = load_lexicon(&file, &BTreeMap::new()).unwrap_err();
        assert_eq!(err.line, Some(3), "{err}");
        assert!(
            err.to_string()
                .starts_with("src/lexicons/com/example/getThing.json:3: invalid JSON"),
            "{err}"
        );
    }

    #[test]
    fn a_byte_order_mark_is_not_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "lexicons/com/example/getThing.json",
            &format!("\u{feff}{}", query("com.example.getThing")),
        );
        assert!(load_lexicon(&file, &BTreeMap::new()).is_ok());
    }

    #[test]
    fn the_upload_s_document_checks_apply() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "lexicons/com/example/getThing.json",
            r#"{"lexicon": 2, "id": "com.example.getThing"}"#,
        );
        let err = load_lexicon(&file, &BTreeMap::new()).unwrap_err();
        assert!(
            err.message.contains("unsupported lexicon version: 2"),
            "{err}"
        );
    }

    #[test]
    fn an_action_override_reaches_the_parsed_lexicon() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "lexicons/com/example/putThing.json",
            &json!({"lexicon": 1, "id": "com.example.putThing", "defs": {"main": {"type": "procedure"}}}).to_string(),
        );
        let overrides = BTreeMap::from([(
            "com.example.putThing".to_string(),
            LexiconOverride {
                action: Some(OverrideAction::Create),
                ..Default::default()
            },
        )]);
        assert_eq!(
            load_lexicon(&file, &overrides).unwrap().parsed.action,
            ProcedureAction::Create
        );
        assert_eq!(
            load_lexicon(&file, &BTreeMap::new()).unwrap().parsed.action,
            ProcedureAction::Upsert
        );
    }

    #[test]
    fn an_override_naming_no_lexicon_file_is_an_error() {
        let overrides = BTreeMap::from([
            (
                "com.example.getThing".to_string(),
                LexiconOverride::default(),
            ),
            ("com.example.gone".to_string(), LexiconOverride::default()),
        ]);
        let files = BTreeSet::from(["com.example.getThing".to_string()]);
        let stale = stale_overrides(&overrides, &files);
        assert_eq!(stale.len(), 1);
        assert_eq!(
            stale[0].to_string(),
            "happyview.toml: [lexicons.\"com.example.gone\"] names no lexicon under src/lexicons/; \
             a stale override is most likely a typo or a rename"
        );
    }

    #[test]
    fn two_files_naming_one_id_are_an_error_naming_both() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(
            &dir,
            "lexicons/com/example.foo.json",
            &query("com.example.foo"),
        );
        let b = write(
            &dir,
            "lexicons/com/example/foo.json",
            &query("com.example.foo"),
        );
        let found = duplicate_ids(&[&a, &b]);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].path,
            PathBuf::from("src/lexicons/com/example/foo.json")
        );
        assert!(
            found[0]
                .message
                .contains("src/lexicons/com/example.foo.json"),
            "{}",
            found[0]
        );
        assert!(duplicate_ids(&[&a]).is_empty());
    }
}
