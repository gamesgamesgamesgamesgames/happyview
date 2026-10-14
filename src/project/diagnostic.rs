//! What a project build reports, file by file.

use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Relative to the project root, with `/` separators.
    pub path: PathBuf,
    pub line: Option<u32>,
    pub message: String,
    pub severity: Severity,
}

impl Diagnostic {
    pub fn error(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            line: None,
            message: message.into(),
            severity: Severity::Error,
        }
    }

    pub fn warning(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            line: None,
            message: message.into(),
            severity: Severity::Warning,
        }
    }

    pub fn at_line(mut self, line: Option<u32>) -> Self {
        self.line = line;
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// `path:line: message`, the shape terminals and editors turn into a link to
/// the file.
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
        }
        match self.severity {
            Severity::Error => write!(f, ": {}", self.message),
            Severity::Warning => write!(f, ": warning: {}", self.message),
        }
    }
}

pub fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(Diagnostic::is_error)
}

/// File order, so a reader meets each file's problems together. A
/// file-level problem sorts before its line-level ones.
pub fn sort(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by(|a, b| (&a.path, a.line, a.severity).cmp(&(&b.path, b.line, b.severity)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diagnostic_prints_as_a_clickable_location() {
        let at_line = Diagnostic::error("src/jobs/a.lua", "boom").at_line(Some(12));
        assert_eq!(at_line.to_string(), "src/jobs/a.lua:12: boom");
        assert_eq!(
            Diagnostic::error("happyview.toml", "bad").to_string(),
            "happyview.toml: bad"
        );
        assert_eq!(
            Diagnostic::warning("src/labels/x.test.lua", "careful").to_string(),
            "src/labels/x.test.lua: warning: careful"
        );
    }

    #[test]
    fn sorting_groups_by_file_then_line_with_errors_first() {
        let mut all = vec![
            Diagnostic::warning("src/b.lua", "w"),
            Diagnostic::error("src/b.lua", "e").at_line(Some(3)),
            Diagnostic::error("src/a.lua", "e"),
            Diagnostic::error("src/b.lua", "e2").at_line(Some(1)),
        ];
        sort(&mut all);
        let shown: Vec<String> = all.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "src/a.lua: e",
                "src/b.lua: warning: w",
                "src/b.lua:1: e2",
                "src/b.lua:3: e"
            ]
        );
        assert!(has_errors(&all));
        assert!(!has_errors(&all[1..2]));
    }
}
