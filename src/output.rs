//! Turning lint results into CLI output: config enums, the per-file report,
//! the gate decision, and the human/JSON renderers. This is the reusable
//! surface a thin binary (or another tool) builds on.

use crate::{Finding, Severity};

/// The version of the JSON output envelope (`schema_version`).
pub const SCHEMA_VERSION: u32 = 1;

/// Minimum finding severity that fails the run (maps to exit code 1).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum Format {
    /// Human-readable text.
    Human,
    /// Machine-readable JSON envelope.
    Json,
}

/// Lint results for a single named input.
#[non_exhaustive]
#[derive(Debug, serde::Serialize)]
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

/// Lint one named input into a [`FileReport`] under `options`, turning a parse
/// failure into the report's `error` field instead of returning an error.
pub fn lint_input(name: impl Into<String>, sql: &str, options: &crate::LintOptions) -> FileReport {
    match crate::lint_sql(sql, options) {
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

#[derive(serde::Serialize)]
struct Report<'a> {
    schema_version: u32,
    files: &'a [FileReport],
}

/// Render one finding as the human block (1–4 newline-terminated lines):
/// the headline, the message, a `fix:` line (unless suppressed), and the
/// snippet (when present).
#[must_use]
pub fn render_finding_human(file: &str, f: &Finding) -> String {
    let suffix = match &f.suppression {
        Some(s) => format!("  — suppressed: {}", s.reason),
        None => String::new(),
    };
    let mut out = format!(
        "{}: {} [{}] statement #{} (line {}, col {}){}\n",
        file, f.severity, f.rule_id, f.statement_index, f.location.line, f.location.column, suffix
    );
    out.push_str(&format!("  {}\n", f.message));
    if f.suppression.is_none() {
        out.push_str(&format!("  fix: {}\n", f.guidance));
    }
    if !f.snippet.is_empty() {
        out.push_str(&format!("  | {}\n", f.snippet));
    }
    out
}

/// Render every finding across `reports` to the human stdout block. Parse
/// errors are not included here — see [`render_errors`].
#[must_use]
pub fn render_human(reports: &[FileReport]) -> String {
    let mut out = String::new();
    for r in reports {
        for f in &r.findings {
            out.push_str(&render_finding_human(&r.name, f));
        }
    }
    out
}

/// Render the stderr block: one `"{name}: {error}"` line for each report that
/// failed to parse.
#[must_use]
pub fn render_errors(reports: &[FileReport]) -> String {
    let mut out = String::new();
    for r in reports {
        if let Some(err) = &r.error {
            out.push_str(&format!("{}: {}\n", r.name, err));
        }
    }
    out
}

/// Render `reports` as the versioned JSON envelope (`schema_version` 1).
///
/// # Errors
/// Returns a message if serialization fails.
pub fn render_json(reports: &[FileReport]) -> Result<String, String> {
    let report = Report {
        schema_version: SCHEMA_VERSION,
        files: reports,
    };
    serde_json::to_string_pretty(&report)
        .map_err(|e| format!("failed to serialize JSON output: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn findings(sql: &str) -> Vec<Finding> {
        crate::lint_sql(sql, &crate::LintOptions::default()).unwrap()
    }

    #[test]
    fn lint_input_reports_findings_for_valid_sql() {
        let r = lint_input(
            "m.sql",
            "CREATE INDEX i ON t (x);",
            &crate::LintOptions::default(),
        );
        assert_eq!(r.name, "m.sql");
        assert!(r.error.is_none());
        assert!(r
            .findings
            .iter()
            .any(|f| f.rule_id == "add-index-non-concurrent"));
    }

    #[test]
    fn lint_input_captures_parse_errors() {
        let r = lint_input("bad.sql", "ALTER TABLE;", &crate::LintOptions::default());
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

    #[test]
    fn render_finding_human_has_id_severity_and_fix() {
        let f = &lint_input("m.sql", "VACUUM FULL t;", &crate::LintOptions::default()).findings[0];
        let s = render_finding_human("m.sql", f);
        assert!(s.contains("error [vacuum-full-cluster]"));
        assert!(s.contains("  fix: "));
    }

    #[test]
    fn render_json_is_the_versioned_envelope() {
        let reports = vec![lint_input(
            "<stdin>",
            "CREATE INDEX i ON t (x);",
            &crate::LintOptions::default(),
        )];
        let s = render_json(&reports).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["files"][0]["file"], "<stdin>");
        assert_eq!(
            v["files"][0]["findings"][0]["rule_id"],
            "add-index-non-concurrent"
        );
    }

    #[test]
    fn render_errors_lists_parse_failures_only() {
        let reports = vec![
            lint_input(
                "ok.sql",
                "CREATE INDEX CONCURRENTLY i ON t (x);",
                &crate::LintOptions::default(),
            ),
            lint_input("bad.sql", "ALTER TABLE;", &crate::LintOptions::default()),
        ];
        let s = render_errors(&reports);
        assert!(s.contains("bad.sql: parse error"));
        assert!(!s.contains("ok.sql"));
    }
}
