//! What `happyview dev` prints. Plain lines on stdout, because `hpy` passes
//! the output through and a terminal turns `path:line:` into a link.

use std::time::Duration;

use crate::dev::DevSession;
use crate::project::Diagnostic;

pub const STILL_SERVING: &str = "build failed; still serving the previous build";

pub fn built(changes: &[String], warnings: &[Diagnostic], elapsed: Duration) -> String {
    let what = if changes.is_empty() {
        "no changes".to_string()
    } else {
        changes.join(", ")
    };
    with_warnings(
        format!("built in {} ms: {what}", elapsed.as_millis()),
        warnings,
    )
}

pub fn booted(warnings: &[Diagnostic], elapsed: Duration) -> String {
    with_warnings(format!("built in {} ms", elapsed.as_millis()), warnings)
}

fn with_warnings(mut line: String, warnings: &[Diagnostic]) -> String {
    for warning in warnings {
        line.push_str(&format!("\n{warning}"));
    }
    line
}

pub fn failed(diagnostics: &[Diagnostic]) -> String {
    let mut out = String::new();
    for diagnostic in diagnostics {
        out.push_str(&format!("{diagnostic}\n"));
    }
    out.push_str(STILL_SERVING);
    out
}

/// Said once per run because it is the consequence of dev mode most easily
/// missed: the developer's real PDS accepts the write, Jetstream broadcasts
/// it, and their production instance indexes it.
pub fn network_warning(network: Option<&str>) -> Option<&'static str> {
    if network == Some("local") {
        return None;
    }
    Some(
        "writes from this instance go to your real PDS and the live network, where any \
         instance indexing the same collections, including your production one, will \
         index them; use `hpy happyview dev --network local` to keep test writes private",
    )
}

pub fn banner(session: &DevSession) -> String {
    format!(
        "happyview dev: serving {} at {}\ndashboard: {}/dashboard/",
        session.root.display(),
        session.public_url,
        session.public_url.trim_end_matches('/'),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Diagnostic;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn a_build_is_one_line_listing_its_changes() {
        let out = built(
            &[
                "added lexicon com.example.thing".into(),
                "changed script job.run:sync".into(),
            ],
            &[],
            Duration::from_millis(42),
        );
        assert_eq!(
            out,
            "built in 42 ms: added lexicon com.example.thing, changed script job.run:sync"
        );
    }

    #[test]
    fn a_build_with_no_changes_says_so() {
        assert_eq!(
            built(&[], &[], Duration::from_millis(3)),
            "built in 3 ms: no changes"
        );
    }

    #[test]
    fn warnings_follow_the_build_line() {
        let out = built(
            &[],
            &[Diagnostic::warning(
                "src/labels/a.lua",
                "shadows a dashboard script",
            )],
            Duration::ZERO,
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[1],
            "src/labels/a.lua: warning: shadows a dashboard script"
        );
    }

    #[test]
    fn a_boot_line_has_no_change_list() {
        assert_eq!(booted(&[], Duration::from_millis(7)), "built in 7 ms");
    }

    #[test]
    fn a_failure_lists_every_error_then_the_standing_line() {
        let out = failed(&[
            Diagnostic::error("src/lexicons/a.json", "expected `,`").at_line(Some(3)),
            Diagnostic::error("src/jobs/b.lua", "unexpected symbol").at_line(Some(12)),
        ]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].starts_with("src/lexicons/a.json:3"),
            "{}",
            lines[0]
        );
        assert!(lines[1].starts_with("src/jobs/b.lua:12"), "{}", lines[1]);
        assert_eq!(lines[2], STILL_SERVING);
    }

    #[test]
    fn the_network_warning_is_skipped_only_on_the_local_network() {
        assert!(network_warning(None).is_some());
        assert!(network_warning(Some("real")).is_some());
        assert!(network_warning(Some("local")).is_none());
    }

    #[test]
    fn the_banner_names_the_project_and_the_dashboard() {
        let session = DevSession {
            root: PathBuf::from("/work/app"),
            public_url: "http://127.0.0.1:3000".into(),
        };
        assert_eq!(
            banner(&session),
            "happyview dev: serving /work/app at http://127.0.0.1:3000\n\
             dashboard: http://127.0.0.1:3000/dashboard/"
        );
    }
}
