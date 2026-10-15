//! What a script file says about itself, and what `happyview.toml` says
//! about it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::config::project::{PROJECT_FILE, ScriptOverride};
use crate::script::checks::MAX_DESCRIPTION_LEN;

use super::diagnostic::Diagnostic;

#[derive(Debug)]
pub struct ScriptMeta {
    pub description: Option<String>,
    pub outbound_xrpcs: Vec<String>,
}

/// The first paragraph of the comment block a file opens with, which is
/// where a reader looks for what a file is for.
pub fn leading_description(source: &str) -> Option<String> {
    leading_comment(source).map(|(description, _)| description)
}

/// `leading_description` and the 1-based line its comment starts on.
fn leading_comment(source: &str) -> Option<(String, u32)> {
    let mut text = source;
    let mut line = 1u32;
    if text.starts_with("#!") {
        text = text.split_once('\n').map_or("", |(_, rest)| rest);
        line += 1;
    }
    let trimmed = text.trim_start();
    line += text[..text.len() - trimmed.len()].matches('\n').count() as u32;
    let text = trimmed;
    let lines: Vec<String> = if let Some((level, rest)) = long_bracket(text) {
        let close = format!("]{}]", "=".repeat(level));
        let body = rest
            .split_once(close.as_str())
            .map_or(rest, |(body, _)| body);
        body.lines().map(block_line).collect()
    } else if let Some(rest) = text.strip_prefix("/*") {
        let body = rest.split_once("*/").map_or(rest, |(body, _)| body);
        body.lines().map(block_line).collect()
    } else {
        text.lines()
            .map_while(|line| {
                let line = line.trim_start();
                line.strip_prefix("--")
                    .or_else(|| line.strip_prefix("//"))
                    .map(|rest| rest.trim_start_matches(['-', '/', '!']).trim().to_string())
            })
            .collect()
    };
    let paragraph: Vec<&str> = lines
        .iter()
        .map(|line| line.trim())
        .skip_while(|line| line.is_empty())
        .take_while(|line| !line.is_empty())
        .collect();
    (!paragraph.is_empty()).then(|| (paragraph.join(" "), line))
}

/// `--[[`, `--[=[`, `--[==[`...: the number of `=` and the text after the
/// opening bracket.
fn long_bracket(text: &str) -> Option<(usize, &str)> {
    let rest = text.strip_prefix("--[")?;
    let level = rest.len() - rest.trim_start_matches('=').len();
    let rest = rest[level..].strip_prefix('[')?;
    Some((level, rest))
}

fn block_line(line: &str) -> String {
    line.trim().trim_start_matches('*').trim().to_string()
}

pub fn script_meta(
    trigger_id: &str,
    path: &Path,
    source: &str,
    overrides: Option<&ScriptOverride>,
) -> Result<ScriptMeta, Diagnostic> {
    let configured = overrides.and_then(|o| o.description.clone());
    let from_config = configured.is_some();
    let leading = leading_comment(source);
    let comment_line = leading.as_ref().map(|(_, line)| *line);
    let description = configured
        .or_else(|| leading.map(|(description, _)| description))
        .filter(|d| !d.is_empty());

    // The admin API's limit, so a description reads the same wherever the
    // dashboard shows it.
    if let Some(description) = &description
        && description.len() > MAX_DESCRIPTION_LEN
    {
        return Err(if from_config {
            Diagnostic::error(
                PROJECT_FILE,
                format!(
                    "[scripts.\"{trigger_id}\"] description must be at most {MAX_DESCRIPTION_LEN} characters"
                ),
            )
        } else {
            Diagnostic::error(
                path,
                format!(
                    "the leading comment's first paragraph is this script's description, which must be at most \
                     {MAX_DESCRIPTION_LEN} characters; shorten it, or set [scripts.\"{trigger_id}\"] description in {PROJECT_FILE}"
                ),
            )
            .at_line(comment_line)
        });
    }

    let outbound_xrpcs = overrides
        .and_then(|o| o.outbound_xrpcs.clone())
        .unwrap_or_else(|| crate::lua_analysis::extract_outbound_xrpcs(source));
    Ok(ScriptMeta {
        description,
        outbound_xrpcs,
    })
}

/// `[scripts."..."]` keys naming no handler file, for the same reason as a
/// stale lexicon override.
pub fn stale_overrides(
    overrides: &BTreeMap<String, ScriptOverride>,
    handled: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    overrides
        .keys()
        .filter(|trigger| !handled.contains(*trigger))
        .map(|trigger| {
            Diagnostic::error(
                PROJECT_FILE,
                format!(
                    "[scripts.\"{trigger}\"] names no script under src/; \
                     a stale override is most likely a typo or a rename"
                ),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::project::ScriptOverride;
    use std::path::PathBuf;

    #[test]
    fn the_first_paragraph_of_a_leading_comment_is_the_description() {
        let cases = [
            (
                "-- Answers a thing.\n-- By id.\n--\n-- Details.\nfunction handle() end",
                Some("Answers a thing. By id."),
            ),
            ("--- Lua doc comment.\nreturn {}", Some("Lua doc comment.")),
            (
                "--[[\n  Block comment\n  over lines.\n\n  More.\n]]\nfunction handle() end",
                Some("Block comment over lines."),
            ),
            (
                "// Answers a thing.\nexport function handle() {}",
                Some("Answers a thing."),
            ),
            (
                "/**\n * Answers a thing.\n *\n * More.\n */\nexport function handle() {}",
                Some("Answers a thing."),
            ),
            (
                "#!/usr/bin/env lua\n-- After a shebang.\n",
                Some("After a shebang."),
            ),
            (
                "\n\n  -- Indented, after blank lines.\n",
                Some("Indented, after blank lines."),
            ),
            ("--[[ One line. ]]\nreturn {}", Some("One line.")),
            (
                "--[==[\n  Long bracket ]] with a decoy.\n]==]\nreturn {}",
                Some("Long bracket ]] with a decoy."),
            ),
            ("--[=[ Level one. ]=]\nreturn {}", Some("Level one.")),
            ("function handle() end\n-- Too late.\n", None),
            ("--\n--\nfunction handle() end", None),
            ("", None),
        ];
        for (source, expected) in cases {
            assert_eq!(
                leading_description(source).as_deref(),
                expected,
                "{source:?}"
            );
        }
    }

    #[test]
    fn an_overlong_description_points_at_the_comment_not_line_one() {
        let long = "x".repeat(MAX_DESCRIPTION_LEN + 1);
        let source = format!("#!/usr/bin/env lua\n\n-- {long}\n");
        let err = script_meta("job.run:a", Path::new("src/jobs/a.lua"), &source, None).unwrap_err();
        assert_eq!(err.line, Some(3), "{err}");
    }

    #[test]
    fn the_config_wins_over_the_comment() {
        let overrides = ScriptOverride {
            description: Some("From config.".into()),
            ..Default::default()
        };
        let meta = script_meta(
            "job.run:a",
            Path::new("src/jobs/a.lua"),
            "-- From comment.\n",
            Some(&overrides),
        )
        .unwrap();
        assert_eq!(meta.description.as_deref(), Some("From config."));

        let meta = script_meta(
            "job.run:a",
            Path::new("src/jobs/a.lua"),
            "-- From comment.\n",
            None,
        )
        .unwrap();
        assert_eq!(meta.description.as_deref(), Some("From comment."));
    }

    #[test]
    fn outbound_xrpcs_are_found_as_the_admin_api_finds_them_unless_declared() {
        let source = "local x = xrpc.query(\"com.example.remote\")\n";
        let found = script_meta("job.run:a", Path::new("src/jobs/a.lua"), source, None).unwrap();
        assert_eq!(
            found.outbound_xrpcs,
            crate::lua_analysis::extract_outbound_xrpcs(source)
        );

        let overrides = ScriptOverride {
            outbound_xrpcs: Some(vec!["com.example.declared".into()]),
            ..Default::default()
        };
        let declared = script_meta(
            "job.run:a",
            Path::new("src/jobs/a.lua"),
            source,
            Some(&overrides),
        )
        .unwrap();
        assert_eq!(declared.outbound_xrpcs, ["com.example.declared"]);
    }

    #[test]
    fn a_description_over_the_limit_points_at_where_it_came_from() {
        let long = "x".repeat(MAX_DESCRIPTION_LEN + 1);
        let err = script_meta(
            "job.run:a",
            Path::new("src/jobs/a.lua"),
            &format!("-- {long}\n"),
            None,
        )
        .unwrap_err();
        assert_eq!(err.to_string().split(':').next(), Some("src/jobs/a.lua"));
        assert!(err.message.contains("300"), "{err}");

        let overrides = ScriptOverride {
            description: Some(long),
            ..Default::default()
        };
        let err = script_meta(
            "job.run:a",
            Path::new("src/jobs/a.lua"),
            "",
            Some(&overrides),
        )
        .unwrap_err();
        assert_eq!(err.path, PathBuf::from("happyview.toml"));
        assert!(err.message.contains(r#"[scripts."job.run:a"]"#), "{err}");
    }

    #[test]
    fn an_override_naming_no_handler_file_is_an_error() {
        let overrides = BTreeMap::from([
            ("job.run:a".to_string(), ScriptOverride::default()),
            ("job.run:gone".to_string(), ScriptOverride::default()),
        ]);
        let handled = BTreeSet::from(["job.run:a".to_string()]);
        let stale = stale_overrides(&overrides, &handled);
        assert_eq!(stale.len(), 1);
        assert!(
            stale[0].to_string().starts_with(
                "happyview.toml: [scripts.\"job.run:gone\"] names no script under src/"
            ),
            "{}",
            stale[0]
        );
    }
}
