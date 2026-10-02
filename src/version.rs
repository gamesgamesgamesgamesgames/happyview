//! The single source of truth for the running HappyView version.
pub fn version() -> &'static str {
    match option_env!("HAPPYVIEW_VERSION") {
        Some(v) if !v.trim().is_empty() => v.trim().trim_start_matches('v'),
        _ => env!("CARGO_PKG_VERSION"),
    }
}

/// The `User-Agent` HappyView sends on outbound requests.
pub fn user_agent() -> String {
    format!("HappyView/{}", version())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_never_empty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn version_has_no_leading_v() {
        assert!(!version().starts_with('v'), "got {}", version());
    }

    #[test]
    fn user_agent_carries_the_resolved_version() {
        assert_eq!(user_agent(), format!("HappyView/{}", version()));
    }

    /// The regression this module exists to prevent: a release build must
    /// not report the never-bumped package version.
    ///
    /// The `assert_ne` is the load-bearing half. Asserting only that
    /// `version()` equals the stamp restates the implementation and would
    /// pass even if the resolution were reversed; this fails if a stamped
    /// build still reports `CARGO_PKG_VERSION`, which is the actual bug.
    #[test]
    fn stamped_builds_do_not_report_the_package_version() {
        let stamped = option_env!("HAPPYVIEW_VERSION")
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let Some(stamped) = stamped else {
            // Unstamped local build: assert the fallback, so this test is
            // never silently vacuous.
            assert_eq!(version(), env!("CARGO_PKG_VERSION"));
            return;
        };

        let expected = stamped.trim_start_matches('v');
        eprintln!(
            "resolved version = {:?} (stamp {:?}, package {:?})",
            version(),
            stamped,
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(version(), expected);
        if expected != env!("CARGO_PKG_VERSION") {
            assert_ne!(
                version(),
                env!("CARGO_PKG_VERSION"),
                "stamped build is still reporting the never-bumped package version"
            );
        }
    }

    /// The bug this module was created to fix: telemetry read
    /// `CARGO_PKG_VERSION` directly and so reported `0.1.0` from every
    /// release build for the life of the project. Six other surfaces had
    /// the same defect, including the outbound User-Agent.
    ///
    /// A doc comment saying "don't do this" is not enforcement. This walks
    /// the source and fails if the constant is read anywhere but here.
    #[test]
    fn cargo_pkg_version_is_read_only_by_this_module() {
        fn walk(dir: &std::path::Path, hits: &mut Vec<String>) {
            let entries = std::fs::read_dir(dir).expect("readable source dir");
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, hits);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && path.file_name().is_some_and(|n| n != "version.rs")
                {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    for (i, line) in text.lines().enumerate() {
                        if line.contains("CARGO_PKG_VERSION") {
                            hits.push(format!("{}:{}", path.display(), i + 1));
                        }
                    }
                }
            }
        }

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut hits = Vec::new();
        walk(&src, &mut hits);
        assert!(
            hits.is_empty(),
            "CARGO_PKG_VERSION reports 0.1.0 in release builds; call \
             crate::version::version() instead. Found at:\n  {}",
            hits.join("\n  ")
        );
    }
}
