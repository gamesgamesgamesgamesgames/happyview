//! The codemod corpus: one `NAME.lua` and the `NAME.expected.lua` a person
//! wrote by hand. The expected files are the specification; the code is made
//! to match them, never the other way round.

use std::path::PathBuf;

use happyview::codemod::{ScriptKind, needs_migration, rewrite};

/// Inputs `needs_migration` says nothing about: a `handle` with no parameters
/// is a contract change that names no global, and a name the trigger does not
/// set is not a global this kind of script ever had.
const NO_GLOBALS: [&str; 2] = [
    "query.handle_no_params",
    "query.marker_global_from_another_kind",
];

/// A note without a `-- codemod:` line above the code it is about leaves the
/// person reading the script nothing to find.
#[test]
fn every_note_has_a_marker_standing_in_the_source() {
    for case in cases() {
        let result = rewrite(&case.source, case.kind).expect("rewrite");
        for note in &result.notes {
            let marker = format!("-- codemod: {}", note.message);
            assert!(
                result.source.lines().any(|line| line.trim() == marker),
                "{}: note at line {} has no marker",
                case.name,
                note.line
            );
        }
    }
}

struct Case {
    name: String,
    kind: ScriptKind,
    source: String,
    expected: String,
}

/// The kind is the file name's first segment, because a case is only
/// meaningful against the trigger it was written for. The `unknown` prefix,
/// which the CLI's `--kind` refuses, exercises the trigger-id fallback the
/// endpoint reaches for a stored id it cannot classify.
fn kind_of(name: &str) -> ScriptKind {
    let prefix = name.split('.').next().unwrap_or_default();
    if prefix == "unknown" {
        return ScriptKind::Unknown;
    }
    ScriptKind::parse(prefix).unwrap_or_else(|| panic!("{name} has no script kind"))
}

fn cases() -> Vec<Case> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/codemod/cases");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("corpus directory")
        .map(|entry| entry.expect("corpus entry").path())
        .collect();
    paths.sort();
    paths
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            let name = name.strip_suffix(".lua")?;
            if name.ends_with(".expected") {
                return None;
            }
            Some(Case {
                name: name.to_string(),
                kind: kind_of(name),
                source: std::fs::read_to_string(path).expect("case source"),
                expected: std::fs::read_to_string(dir.join(format!("{name}.expected.lua")))
                    .unwrap_or_else(|_| panic!("{name} has no expected file")),
            })
        })
        .collect()
}

#[test]
fn the_corpus_covers_the_rewrite_table() {
    assert!(cases().len() >= 25, "{} cases", cases().len());
}

#[test]
fn every_case_rewrites_to_its_expected_file() {
    let mut failures = Vec::new();
    for case in cases() {
        let got = rewrite(&case.source, case.kind).expect("rewrite").source;
        if got != case.expected {
            failures.push(format!(
                "--- {} ---\n-- got --\n{got}\n-- want --\n{}",
                case.name, case.expected
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_case_is_idempotent() {
    for case in cases() {
        let once = rewrite(&case.source, case.kind).expect("rewrite").source;
        let twice = rewrite(&once, case.kind).expect("rewrite again").source;
        assert_eq!(once, twice, "{}", case.name);
    }
}

#[test]
fn every_expected_file_is_loadable_lua() {
    let lua = happyview::lua::sandbox_for_tests();
    // The installed `luac` is 5.5 while the sandbox is 5.4, so this gate is
    // stricter than the runtime: 5.5 refuses an assignment to a for-loop
    // variable that 5.4 accepts. Skipped where no `luac` is on the path.
    let luac = std::process::Command::new("luac")
        .arg("-v")
        .output()
        .is_ok();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/codemod/cases");
    for case in cases() {
        assert!(
            full_moon::parse(&case.expected).is_ok(),
            "{} does not parse",
            case.name
        );
        // Syntax only: loading compiles the chunk without running it, so a
        // rewrite that produced an unbalanced chain is caught here rather
        // than in production.
        lua.load(case.expected.as_str())
            .into_function()
            .unwrap_or_else(|e| panic!("{} does not compile: {e}", case.name));
        lua.load(case.source.as_str())
            .into_function()
            .unwrap_or_else(|e| panic!("{} is not valid to begin with: {e}", case.name));
        if luac {
            let status = std::process::Command::new("luac")
                .arg("-p")
                .arg(dir.join(format!("{}.expected.lua", case.name)))
                .status()
                .expect("run luac");
            assert!(status.success(), "{} is refused by luac -p", case.name);
        }
    }
}

#[test]
fn needs_migration_empties_out_over_a_clean_rewrite() {
    for case in cases() {
        let before = needs_migration(&case.source, case.kind);
        let after = needs_migration(&case.expected, case.kind);
        let notes = rewrite(&case.source, case.kind).expect("rewrite").notes;

        if case.source != case.expected && !NO_GLOBALS.contains(&case.name.as_str()) {
            assert!(!before.is_empty(), "{}: nothing to migrate", case.name);
        }
        if notes.is_empty() {
            assert!(after.is_empty(), "{}: {after:?} left over", case.name);
        }
        for left in &after {
            assert!(
                before.contains(left),
                "{}: {left} appeared during the rewrite",
                case.name
            );
        }
    }
}

#[test]
fn a_marked_case_names_every_construct_it_left_behind() {
    for case in cases() {
        let result = rewrite(&case.source, case.kind).expect("rewrite");
        let markers = result
            .source
            .lines()
            .filter(|line| line.trim_start().starts_with("-- codemod:"))
            .count();
        let original = case
            .source
            .lines()
            .filter(|line| line.trim_start().starts_with("-- codemod:"))
            .count();
        assert_eq!(
            markers - original,
            result.notes.len(),
            "{}: markers and notes disagree",
            case.name
        );
    }
}
