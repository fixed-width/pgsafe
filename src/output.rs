//! Turning lint results into CLI output: config enums, the per-file report,
//! the gate decision, and the human/JSON renderers. This is the reusable
//! surface a thin binary (or another tool) builds on.

use crate::{Finding, Severity};

/// Minimum finding severity that fails the run (maps to exit code 1).
#[non_exhaustive]
#[derive(Clone, Copy)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum FailOn {
    /// Fail only on error-severity findings.
    Error,
    /// Fail on any finding (warning or error). This is the default.
    Warning,
    /// Never fail on findings (report-only).
    Never,
}

/// Output format for the CLI.
#[non_exhaustive]
#[derive(Clone)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum Format {
    /// Human-readable text.
    Human,
    /// Machine-readable JSON envelope.
    Json,
}

/// Lint results for a single named input.
#[non_exhaustive]
#[derive(serde::Serialize)]
pub struct FileReport {
    /// The input's name (a file path, or `<stdin>`). Serialized as `"file"`.
    #[serde(rename = "file")]
    pub name: String,
    /// Findings for this input, in source order.
    pub findings: Vec<Finding>,
    /// `Some` when the input could not be parsed; `findings` is then empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Lint one named input into a [`FileReport`], turning a parse failure into the
/// report's `error` field instead of returning an error.
pub fn lint_input(name: impl Into<String>, sql: &str) -> FileReport {
    match crate::lint_sql(sql) {
        Ok(findings) => FileReport {
            name: name.into(),
            findings,
            error: None,
        },
        Err(e) => FileReport {
            name: name.into(),
            findings: Vec::new(),
            error: Some(e.to_string()),
        },
    }
}

/// Whether `findings` should fail the run under `fail_on`.
///
/// Suppressed findings never gate. This is the single definition of the
/// `--fail-on` decision; the exit-code precedence (parse error > finding) is
/// the caller's.
#[must_use]
pub fn gate(findings: &[Finding], fail_on: FailOn) -> bool {
    let min = match fail_on {
        FailOn::Never => return false,
        FailOn::Warning => Severity::Warning,
        FailOn::Error => Severity::Error,
    };
    findings
        .iter()
        .any(|f| !f.is_suppressed() && f.severity >= min)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn findings(sql: &str) -> Vec<Finding> {
        crate::lint_sql(sql).unwrap()
    }

    #[test]
    fn lint_input_reports_findings_for_valid_sql() {
        let r = lint_input("m.sql", "CREATE INDEX i ON t (x);");
        assert_eq!(r.name, "m.sql");
        assert!(r.error.is_none());
        assert!(r
            .findings
            .iter()
            .any(|f| f.rule_id == "add-index-non-concurrent"));
    }

    #[test]
    fn lint_input_captures_parse_errors() {
        let r = lint_input("bad.sql", "ALTER TABLE;");
        assert!(r.findings.is_empty());
        assert!(r.error.as_deref().unwrap().contains("parse error"));
    }

    #[test]
    fn never_never_gates() {
        assert!(!gate(&findings("VACUUM FULL t;"), FailOn::Never));
    }

    #[test]
    fn warning_gates_on_a_warning() {
        // DROP TABLE is a warning-severity rule.
        assert!(gate(&findings("DROP TABLE x;"), FailOn::Warning));
    }

    #[test]
    fn error_does_not_gate_on_a_warning_but_does_on_an_error() {
        assert!(!gate(&findings("DROP TABLE x;"), FailOn::Error));
        assert!(gate(&findings("VACUUM FULL t;"), FailOn::Error));
    }

    #[test]
    fn no_findings_never_gates() {
        assert!(!gate(
            &findings("CREATE INDEX CONCURRENTLY i ON t (x);"),
            FailOn::Warning
        ));
    }
}
