//! Turning lint results into CLI output: config enums, the per-file report,
//! the gate decision, and the human/JSON renderers. This is the reusable
//! surface a thin binary (or another tool) builds on.

use crate::{Finding, Location, Severity};

/// The version of the JSON output envelope (`schema_version`).
pub const SCHEMA_VERSION: u32 = 1;

/// Minimum finding severity that fails the run (maps to exit code 1).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum Format {
    /// Human-readable text.
    Human,
    /// Machine-readable JSON envelope.
    Json,
    /// GitHub Actions workflow annotation commands (for the GitHub Action / CI).
    Github,
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

/// Render the grouped-output header for a statement: `{file}:{line}:{col}  {snippet}`
/// (the snippet is omitted when empty). One header precedes all the findings on that statement.
#[must_use]
pub fn render_statement_header(file: &str, location: Location, snippet: &str) -> String {
    if snippet.is_empty() {
        format!("{file}:{}:{}\n", location.line, location.column)
    } else {
        format!("{file}:{}:{}  {snippet}\n", location.line, location.column)
    }
}

/// Render one finding's nested body for the grouped output: a `  {severity} [{rule}]` line
/// (with a suppression note when suppressed), the indented message, and a `fix:` line unless
/// suppressed. The owning statement's location and snippet come from [`render_statement_header`].
#[must_use]
pub fn render_finding_body(f: &Finding) -> String {
    let suffix = match &f.suppression {
        Some(s) => format!("  — suppressed: {}", s.reason),
        None => String::new(),
    };
    let mut out = format!("  {} [{}]{}\n", f.severity, f.rule_id, suffix);
    out.push_str(&format!("    {}\n", f.message));
    if f.suppression.is_none() {
        out.push_str(&format!("    fix: {}\n", f.guidance));
    }
    out
}

/// Render every finding across `reports` to the human stdout block, grouped by statement: each
/// statement's location and snippet appear once as a header, its findings nested beneath, with a
/// blank line between statements. Findings arrive sorted by statement index, so a group is the run
/// of consecutive findings sharing one index. Parse errors are not included — see [`render_errors`].
#[must_use]
pub fn render_human(reports: &[FileReport]) -> String {
    let mut out = String::new();
    let mut first = true;
    for r in reports {
        let mut i = 0;
        while i < r.findings.len() {
            if !first {
                out.push('\n');
            }
            first = false;
            let head = &r.findings[i];
            out.push_str(&render_statement_header(
                &r.name,
                head.location,
                &head.snippet,
            ));
            let stmt = head.statement_index;
            while i < r.findings.len() && r.findings[i].statement_index == stmt {
                out.push_str(&render_finding_body(&r.findings[i]));
                i += 1;
            }
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

/// Escape a workflow-command message value: `%`, CR, LF.
fn gh_escape_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Escape a workflow-command property value: the data escapes plus `,` and `:`.
fn gh_escape_prop(s: &str) -> String {
    gh_escape_data(s).replace(',', "%2C").replace(':', "%3A")
}

/// Render findings as GitHub Actions workflow annotation commands (`--format github`): one
/// `::error`/`::warning file=…,line=…,col=…,title=pgsafe(rule)::message` per finding (suppressed
/// findings skipped), plus `::error file=…::{error}` for any file that failed to parse. GitHub turns
/// these into inline annotations on the diff; the process exit code (the gate) still drives the check.
#[must_use]
pub fn render_github(reports: &[FileReport]) -> String {
    let mut out = String::new();
    for r in reports {
        if let Some(err) = &r.error {
            out.push_str(&format!(
                "::error file={}::{}\n",
                gh_escape_prop(&r.name),
                gh_escape_data(err)
            ));
            continue;
        }
        for f in &r.findings {
            if f.is_suppressed() {
                continue;
            }
            let level = match f.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            };
            out.push_str(&format!(
                "::{level} file={},line={},col={},title={}::{}\n",
                gh_escape_prop(&r.name),
                f.location.line,
                f.location.column,
                gh_escape_prop(&format!("pgsafe({})", f.rule_id)),
                gh_escape_data(&f.message),
            ));
        }
    }
    out
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
    fn render_human_groups_findings_by_statement() {
        let reports = vec![lint_input(
            "m.sql",
            "DROP TABLE users;\nCREATE INDEX i ON t (x);",
            &crate::LintOptions::default(),
        )];
        let s = render_human(&reports);
        // Each statement has exactly one header line with its snippet (the two findings on
        // statement 0 — drop-table + require-timeout — share one header).
        assert_eq!(
            s.matches("m.sql:").count(),
            2,
            "one header per statement:\n{s}"
        );
        assert!(s.contains("m.sql:1:1  DROP TABLE users\n"));
        assert!(s.contains("m.sql:2:1  CREATE INDEX i ON t (x)\n"));
        // Findings are nested (indented) under their statement header.
        assert!(s.contains("\n  warning [drop-table]\n    "));
        assert!(s.contains("\n  error [add-index-non-concurrent]\n    "));
        // A blank line separates the two statement groups.
        assert!(
            s.contains("\n\nm.sql:2:1"),
            "blank line between groups:\n{s}"
        );
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

    #[test]
    fn gh_escape_helpers_encode_special_chars() {
        assert_eq!(super::gh_escape_data("a%b\nc\rd"), "a%25b%0Ac%0Dd");
        assert_eq!(super::gh_escape_prop("x,y:z"), "x%2Cy%3Az");
    }

    #[test]
    fn render_github_emits_severity_keyed_annotations() {
        // VACUUM FULL → an error rule (vacuum-full-cluster) + a warning (require-timeout), both at 1:1.
        let reports = vec![lint_input(
            "m.sql",
            "VACUUM FULL t;",
            &crate::LintOptions::default(),
        )];
        let s = render_github(&reports);
        assert!(s.contains("::error file=m.sql,line=1,col=1,title=pgsafe(vacuum-full-cluster)::"));
        assert!(s.contains("::warning file=m.sql,line=1,col=1,title=pgsafe(require-timeout)::"));
    }

    #[test]
    fn render_github_skips_suppressed_findings() {
        let sql = "-- pgsafe:ignore vacuum-full-cluster reviewed\nVACUUM FULL t;";
        let reports = vec![lint_input("m.sql", sql, &crate::LintOptions::default())];
        let s = render_github(&reports);
        // the suppressed finding is not annotated; an unsuppressed sibling still is.
        assert!(!s.contains("title=pgsafe(vacuum-full-cluster)"));
        assert!(s.contains("title=pgsafe(require-timeout)"));
    }

    #[test]
    fn render_github_annotates_a_parse_error() {
        let reports = vec![lint_input(
            "bad.sql",
            "ALTER TABLE;",
            &crate::LintOptions::default(),
        )];
        let s = render_github(&reports);
        assert!(s.starts_with("::error file=bad.sql::"), "got: {s}");
        assert!(s.contains("parse error"));
    }
}
