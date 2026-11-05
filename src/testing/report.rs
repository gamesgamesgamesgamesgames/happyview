//! What a run prints, and what it exits with.

use std::io::Write;
use std::time::Duration;

use crate::plugin::{TestReport, TestStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub message: String,
    /// The chunk the error happened in, when the interpreter placed it.
    pub file: Option<String>,
    pub line: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct FileResult {
    /// The test file's project-relative path, which is its key.
    pub path: String,
    pub reports: Vec<TestReport>,
    /// The file itself failed: it would not load, raised outside a test, or
    /// reported no tests.
    pub error: Option<FileError>,
    pub duration: Duration,
}

impl FileResult {
    pub fn failed(&self) -> bool {
        self.error.is_some() || self.reports.iter().any(|r| r.status == TestStatus::Fail)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub files: usize,
    pub files_failed: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// `file:line`, falling back to the test file when nothing placed it.
fn place(path: &str, file: Option<&str>, line: Option<u32>) -> String {
    let file = file.unwrap_or(path);
    match line {
        Some(line) => format!("{file}:{line}"),
        None => file.to_string(),
    }
}

fn counts(parts: &[(usize, &str)], total: usize) -> String {
    let shown: Vec<String> = parts
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, label)| format!("{n} {label}"))
        .collect();
    format!("{} ({total})", shown.join(", "))
}

pub fn render(
    results: &[FileResult],
    elapsed: Duration,
    out: &mut dyn Write,
) -> std::io::Result<Summary> {
    let mut summary = Summary::default();
    for result in results {
        summary.files += 1;
        let failed = result
            .reports
            .iter()
            .filter(|r| r.status == TestStatus::Fail)
            .count();
        summary.failed += failed;
        summary.passed += result
            .reports
            .iter()
            .filter(|r| r.status == TestStatus::Pass)
            .count();
        summary.skipped += result
            .reports
            .iter()
            .filter(|r| r.status == TestStatus::Skip)
            .count();
        let verdict = if result.failed() {
            summary.files_failed += 1;
            "FAIL"
        } else {
            "PASS"
        };
        let ms = result.duration.as_millis();
        match (result.reports.len(), failed) {
            (0, _) => writeln!(out, " {verdict}  {} {ms}ms", result.path)?,
            (n, 0) => writeln!(out, " {verdict}  {} ({n} tests) {ms}ms", result.path)?,
            (n, f) => writeln!(
                out,
                " {verdict}  {} ({n} tests, {f} failed) {ms}ms",
                result.path
            )?,
        }
        if let Some(error) = &result.error {
            writeln!(
                out,
                "       {}: {}",
                place(&result.path, error.file.as_deref(), error.line),
                error.message
            )?;
        }
    }

    for result in results {
        for report in result
            .reports
            .iter()
            .filter(|r| r.status == TestStatus::Fail)
        {
            writeln!(out)?;
            writeln!(out, " FAIL  {} > {}", result.path, report.name)?;
            writeln!(
                out,
                "       {}: {}",
                place(&result.path, report.file.as_deref(), report.line),
                report.message.as_deref().unwrap_or("failed")
            )?;
        }
    }

    writeln!(out)?;
    writeln!(
        out,
        "Test files  {}",
        counts(
            &[
                (summary.files_failed, "failed"),
                (summary.files - summary.files_failed, "passed")
            ],
            summary.files
        )
    )?;
    let tests = summary.passed + summary.failed + summary.skipped;
    writeln!(
        out,
        "     Tests  {}",
        counts(
            &[
                (summary.failed, "failed"),
                (summary.passed, "passed"),
                (summary.skipped, "skipped")
            ],
            tests
        )
    )?;
    writeln!(out, "  Duration  {:.2}s", elapsed.as_secs_f64())?;
    Ok(summary)
}

/// A run that ran nothing did not show anything works, so it is not a pass.
pub fn exit_code(summary: &Summary) -> u8 {
    if summary.files == 0 || summary.files_failed > 0 {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::TestStatus;

    const FILE: &str = "src/lib/a.test.lua";

    fn report(name: &str, status: TestStatus, line: Option<u32>) -> TestReport {
        TestReport {
            path: FILE.into(),
            name: name.into(),
            status,
            message: (status == TestStatus::Fail).then(|| "expected 1, got 2".to_string()),
            file: line.map(|_| FILE.to_string()),
            line,
            duration_ms: 1,
        }
    }

    fn render_to_string(results: &[FileResult]) -> (String, Summary) {
        let mut out = Vec::new();
        let summary = render(results, Duration::from_millis(310), &mut out).unwrap();
        (String::from_utf8(out).unwrap(), summary)
    }

    #[test]
    fn a_failure_is_listed_with_its_place_and_counted() {
        let (text, summary) = render_to_string(&[FileResult {
            path: FILE.into(),
            reports: vec![
                report("a > passes", TestStatus::Pass, None),
                report("a > fails", TestStatus::Fail, Some(7)),
                report("a > later", TestStatus::Skip, None),
            ],
            error: None,
            duration: Duration::from_millis(12),
        }]);
        assert!(
            text.contains(" FAIL  src/lib/a.test.lua (3 tests, 1 failed)"),
            "{text}"
        );
        assert!(
            text.contains(
                " FAIL  src/lib/a.test.lua > a > fails\n       src/lib/a.test.lua:7: expected 1, got 2"
            ),
            "{text}"
        );
        assert!(text.contains("Test files  1 failed (1)"), "{text}");
        assert!(
            text.contains("     Tests  1 failed, 1 passed, 1 skipped (3)"),
            "{text}"
        );
        assert_eq!(exit_code(&summary), 1);
    }

    #[test]
    fn a_file_error_is_placed_in_the_chunk_it_happened_in() {
        let (text, summary) = render_to_string(&[FileResult {
            path: "src/jobs/x.test.lua".into(),
            reports: vec![],
            error: Some(FileError {
                message: "attempt to call a nil value".into(),
                file: Some("src/lib/helpers.lua".into()),
                line: Some(4),
            }),
            duration: Duration::from_millis(3),
        }]);
        assert!(
            text.contains(
                " FAIL  src/jobs/x.test.lua 3ms\n       src/lib/helpers.lua:4: attempt to call a nil value"
            ),
            "{text}"
        );
        assert_eq!(summary.files_failed, 1);
        assert_eq!(exit_code(&summary), 1);
    }

    #[test]
    fn all_passing_exits_zero_and_nothing_run_exits_one() {
        let (_, summary) = render_to_string(&[FileResult {
            path: "src/a.test.lua".into(),
            reports: vec![report("ok", TestStatus::Pass, None)],
            error: None,
            duration: Duration::ZERO,
        }]);
        assert_eq!(exit_code(&summary), 0);
        let (_, empty) = render_to_string(&[]);
        assert_eq!(exit_code(&empty), 1);
    }
}
