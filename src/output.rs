//! Turning lint results into CLI output: config enums, the per-file report,
//! the gate decision, and the human/JSON renderers. This is the reusable
//! surface a thin binary (or another tool) builds on.

use crate::{Finding, Location, Severity};
use anstyle::{AnsiColor, Style};
use std::io::IsTerminal;

/// How the human renderers wrap each span of output. `plain()` emits neither
/// ANSI escapes nor glyphs (byte-identical to unstyled output); `ansi()` colors
/// spans and prefixes a per-finding severity glyph. TTY/env detection is the
/// caller's job — the library only consumes a ready-made `Styling`.
#[derive(Debug, Clone)]
pub struct Styling {
    danger: Style,
    warning: Style,
    success: Style,
    accent: Style,
    muted: Style,
    strong: Style,
    glyphs: bool,
}

impl Styling {
    /// No color, no glyphs. Output is byte-identical to the historical plain report.
    #[must_use]
    pub fn plain() -> Self {
        let n = Style::new();
        Self {
            danger: n,
            warning: n,
            success: n,
            accent: n,
            muted: n,
            strong: n,
            glyphs: false,
        }
    }

    /// Colors + per-finding glyphs, for a terminal that supports ANSI.
    #[must_use]
    pub fn ansi() -> Self {
        let fg = |c| Style::new().fg_color(Some(c));
        Self {
            danger: fg(AnsiColor::Red.into()).bold(),
            warning: fg(AnsiColor::Yellow.into()).bold(),
            success: fg(AnsiColor::Green.into()).bold(),
            accent: fg(AnsiColor::Cyan.into()),
            muted: Style::new().dimmed(),
            strong: Style::new().bold(),
            glyphs: true,
        }
    }

    /// Resolve to [`Styling::ansi`] or [`Styling::plain`] for `color` and the
    /// current environment: `CLICOLOR_FORCE` (set, non-empty, ≠"0") forces color,
    /// else `NO_COLOR` (same test) forces plain, else color iff stdout is a
    /// terminal. `Always` overrides `NO_COLOR`; `Never` is always plain. This is
    /// format-agnostic — call it only for human output; keep machine formats plain.
    #[must_use]
    pub fn resolve(color: ColorWhen) -> Styling {
        let on = color_on(
            color,
            env_set("CLICOLOR_FORCE"),
            env_set("NO_COLOR"),
            std::io::stdout().is_terminal(),
        );
        if on {
            Styling::ansi()
        } else {
            Styling::plain()
        }
    }

    /// Paint `s` in the "danger" role (errors, rejects): red + bold.
    #[must_use]
    pub fn danger(&self, s: &str) -> String {
        paint(self.danger, s)
    }

    /// Paint `s` in the "warning" role: yellow + bold.
    #[must_use]
    pub fn warning(&self, s: &str) -> String {
        paint(self.warning, s)
    }

    /// Paint `s` in the "success" role (ready/ok verdicts): green + bold.
    #[must_use]
    pub fn success(&self, s: &str) -> String {
        paint(self.success, s)
    }

    /// Paint `s` in the "accent" role (secondary emphasis, e.g. rewrites): cyan.
    #[must_use]
    pub fn accent(&self, s: &str) -> String {
        paint(self.accent, s)
    }

    /// Paint `s` in the "muted" role (notes, secondary detail): dimmed.
    #[must_use]
    pub fn muted(&self, s: &str) -> String {
        paint(self.muted, s)
    }

    /// Paint `s` in the "strong" role (emphasis without color): bold.
    #[must_use]
    pub fn strong(&self, s: &str) -> String {
        paint(self.strong, s)
    }

    /// Paint `text` in the role for `s`'s severity (error → danger, warning → warning).
    fn severity(&self, s: Severity, text: &str) -> String {
        match s {
            Severity::Error => self.danger(text),
            Severity::Warning => self.warning(text),
        }
    }

    /// The glyph for a severity (`✗`/`⚠`), or `""` when glyphs are disabled.
    fn glyph(&self, s: Severity) -> &'static str {
        if !self.glyphs {
            return "";
        }
        match s {
            Severity::Error => "✗",
            Severity::Warning => "⚠",
        }
    }
}

/// Wrap `text` in `style`'s ANSI escapes. An empty `Style` renders no escapes,
/// so the plain path flows through this unchanged.
fn paint(style: Style, text: &str) -> String {
    format!("{}{}{}", style.render(), text, style.render_reset())
}

/// A conventional color env var counts as "set" when present and neither empty
/// nor `"0"` (so `NO_COLOR=` and `CLICOLOR_FORCE=0` do not trigger).
fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0")
}

/// The pure color decision, factored out of [`Styling::resolve`] so it can be
/// unit-tested without touching process-global env/TTY state.
fn color_on(color: ColorWhen, clicolor_force: bool, no_color: bool, is_tty: bool) -> bool {
    match color {
        ColorWhen::Never => false,
        ColorWhen::Always => true,
        ColorWhen::Auto => {
            if clicolor_force {
                true
            } else if no_color {
                false
            } else {
                is_tty
            }
        }
    }
}

/// The version of the JSON output envelope (`schema_version`).
pub const SCHEMA_VERSION: u32 = 2;

/// When to colorize human output (the CLI `--color` flag).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum ColorWhen {
    /// Colorize when stdout is a terminal, honoring `NO_COLOR` / `CLICOLOR_FORCE`. Default.
    Auto,
    /// Always colorize.
    Always,
    /// Never colorize.
    Never,
}

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
    /// SARIF 2.1.0 (for GitHub code-scanning ingestion).
    Sarif,
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

/// Styled statement header: `{file}:{line}:{col}` (dimmed under `ansi`) then two
/// spaces and the snippet when present. See [`render_statement_header`] for the
/// stable plain form.
fn statement_header(file: &str, location: Location, snippet: &str, st: &Styling) -> String {
    let loc = st.muted(&format!("{file}:{}:{}", location.line, location.column));
    if snippet.is_empty() {
        format!("{loc}\n")
    } else {
        format!("{loc}  {snippet}\n")
    }
}

/// Render the grouped-output header for a statement (plain styling).
#[must_use]
pub fn render_statement_header(file: &str, location: Location, snippet: &str) -> String {
    statement_header(file, location, snippet, &Styling::plain())
}

/// Styled finding body. A suppressed finding renders as a single dimmed block
/// (header line with the suppression note + message, no fix line); dimming the
/// whole block rather than per-span keeps nested resets from cancelling the dim.
/// An unsuppressed finding gets a per-severity glyph, colored severity word,
/// bold rule id, and a dimmed `fix:` line.
fn finding_body(f: &Finding, st: &Styling) -> String {
    if let Some(s) = &f.suppression {
        let block = format!(
            "  {} [{}]  — suppressed: {}\n    {}\n",
            f.severity, f.rule_id, s.reason, f.message
        );
        return st.muted(&block);
    }
    let glyph = st.glyph(f.severity);
    let prefix = if glyph.is_empty() {
        String::new()
    } else {
        format!("{} ", st.severity(f.severity, glyph))
    };
    let mut out = format!(
        "  {}{} [{}]\n",
        prefix,
        st.severity(f.severity, &f.severity.to_string()),
        st.strong(&f.rule_id),
    );
    out.push_str(&format!("    {}\n", f.message));
    out.push_str(&st.muted(&format!("    fix: {}\n", f.guidance)));
    out
}

/// Render one finding's nested body for the grouped output (plain styling).
#[must_use]
pub fn render_finding_body(f: &Finding) -> String {
    finding_body(f, &Styling::plain())
}

/// Render every finding across `reports` to the human block, grouped by statement,
/// wrapping each span according to `st`. Findings arrive sorted by statement index,
/// so a group is the run of consecutive findings sharing one index. Parse errors
/// are not included — see [`render_errors`].
#[must_use]
pub fn render_human_styled(reports: &[FileReport], st: &Styling) -> String {
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
            out.push_str(&statement_header(&r.name, head.location, &head.snippet, st));
            let stmt = head.statement_index;
            while i < r.findings.len() && r.findings[i].statement_index == stmt {
                out.push_str(&finding_body(&r.findings[i], st));
                i += 1;
            }
        }
    }
    out
}

/// Render every finding across `reports` to the plain human block, grouped by statement.
#[must_use]
pub fn render_human(reports: &[FileReport]) -> String {
    render_human_styled(reports, &Styling::plain())
}

/// Count `(errors, warnings, suppressed)` across every report's findings.
/// Suppressed findings are counted only in the third slot (they never gate).
fn tally(reports: &[FileReport]) -> (usize, usize, usize) {
    let (mut errors, mut warnings, mut suppressed) = (0, 0, 0);
    for r in reports {
        for f in &r.findings {
            if f.is_suppressed() {
                suppressed += 1;
            } else if f.severity == Severity::Error {
                errors += 1;
            } else {
                warnings += 1;
            }
        }
    }
    (errors, warnings, suppressed)
}

/// `"{n} {word}"`, appending `s` to pluralize unless `n == 1`.
fn count(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// One-line run summary, or `None` when there are no findings at all (a clean run
/// stays silent). Printed in both plain and color modes; `st` only colors the
/// counts. Clauses with a zero count are omitted; suppressed findings appear in a
/// trailing `(N suppressed)` parenthetical (or stand alone when they are the only
/// findings).
#[must_use]
pub fn render_summary(reports: &[FileReport], st: &Styling) -> Option<String> {
    let (errors, warnings, suppressed) = tally(reports);
    if errors == 0 && warnings == 0 && suppressed == 0 {
        return None;
    }
    let mut clauses = Vec::new();
    if errors > 0 {
        clauses.push(st.danger(&count(errors, "error")));
    }
    if warnings > 0 {
        clauses.push(st.warning(&count(warnings, "warning")));
    }
    let supp = st.muted(&format!("{suppressed} suppressed"));
    let files = count(reports.len(), "file");
    let body = if clauses.is_empty() {
        // A run whose only findings are suppressed.
        format!("{supp} in {files}")
    } else {
        let mut b = clauses.join(", ");
        if suppressed > 0 {
            b.push_str(&format!(" ({supp})"));
        }
        format!("{b} in {files}")
    };
    Some(format!("Summary: {body}"))
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

/// Render `reports` as the versioned JSON envelope (`schema_version` 2).
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
        assert_eq!(v["schema_version"], 2);
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

    #[test]
    fn ansi_paints_escapes_plain_does_not() {
        let ansi = Styling::ansi();
        let plain = Styling::plain();
        assert!(ansi.danger("error").contains('\u{1b}'));
        assert_eq!(plain.danger("error"), "error");
        assert_eq!(ansi.glyph(Severity::Error), "✗");
        assert_eq!(ansi.glyph(Severity::Warning), "⚠");
        assert_eq!(plain.glyph(Severity::Error), "");
    }

    #[test]
    fn role_painters_use_expected_sgr() {
        let a = Styling::ansi();
        // anstyle renders each attribute as its own escape: bold `\x1b[1m`, dim
        // `\x1b[2m`, fg red `\x1b[31m`, yellow `\x1b[33m`, green `\x1b[32m`, cyan `\x1b[36m`.
        assert!(a.danger("x").contains("\u{1b}[1m") && a.danger("x").contains("\u{1b}[31m"));
        assert!(a.warning("x").contains("\u{1b}[33m"));
        assert!(a.success("x").contains("\u{1b}[32m"));
        assert!(a.accent("x").contains("\u{1b}[36m"));
        assert!(a.muted("x").contains("\u{1b}[2m"));
        assert!(a.strong("x").contains("\u{1b}[1m"));
        let p = Styling::plain();
        for painted in [
            p.danger("x"),
            p.warning("x"),
            p.success("x"),
            p.accent("x"),
            p.muted("x"),
            p.strong("x"),
        ] {
            assert_eq!(painted, "x");
        }
    }

    #[test]
    fn summary_counts_are_bold_in_ansi() {
        let reports = vec![lint_input(
            "m.sql",
            "VACUUM FULL t;",
            &crate::LintOptions::default(),
        )];
        let s = render_summary(&reports, &Styling::ansi()).unwrap();
        // Counts now share the danger/warning roles, so they are bold.
        assert!(
            s.contains("\u{1b}[1m"),
            "summary counts should be bold: {s}"
        );
    }

    #[test]
    fn plain_render_is_byte_identical_and_escape_free() {
        let reports = vec![lint_input(
            "m.sql",
            "DROP TABLE users;\nVACUUM FULL t;",
            &crate::LintOptions::default(),
        )];
        let s = render_human(&reports);
        assert!(
            !s.contains('\u{1b}'),
            "plain output must have no escapes:\n{s}"
        );
        assert!(
            !s.contains('✗') && !s.contains('⚠'),
            "plain output must have no glyphs"
        );
        // The plain wrapper and an explicit plain styling agree.
        assert_eq!(s, render_human_styled(&reports, &Styling::plain()));
        // Layout unchanged: header + nested finding lines still present.
        assert!(s.contains("m.sql:1:1  DROP TABLE users\n"));
        assert!(s.contains("\n  warning [drop-table]\n    "));
    }

    #[test]
    fn ansi_render_adds_escapes_and_severity_glyphs() {
        let reports = vec![lint_input(
            "m.sql",
            "VACUUM FULL t;",
            &crate::LintOptions::default(),
        )];
        let s = render_human_styled(&reports, &Styling::ansi());
        assert!(s.contains('\u{1b}'), "ansi output must contain escapes");
        assert!(s.contains('✗'), "vacuum-full-cluster is an error → ✗");
        assert!(s.contains('⚠'), "require-timeout is a warning → ⚠");
    }

    #[test]
    fn summary_is_none_on_a_clean_run() {
        let reports = vec![lint_input(
            "ok.sql",
            "CREATE INDEX CONCURRENTLY i ON t (x);",
            &crate::LintOptions::default(),
        )];
        assert!(render_summary(&reports, &Styling::plain()).is_none());
    }

    #[test]
    fn summary_counts_and_pluralizes() {
        // VACUUM FULL → 1 error (vacuum-full-cluster) + 1 warning (require-timeout).
        let reports = vec![lint_input(
            "m.sql",
            "VACUUM FULL t;",
            &crate::LintOptions::default(),
        )];
        assert_eq!(
            render_summary(&reports, &Styling::plain()).unwrap(),
            "Summary: 1 error, 1 warning in 1 file"
        );
    }

    #[test]
    fn summary_parenthesizes_suppressed_and_colors_in_ansi() {
        let sql = "-- pgsafe:ignore drop-table cleanup\nDROP TABLE x;\nVACUUM FULL t;";
        let reports = vec![lint_input("m.sql", sql, &crate::LintOptions::default())];
        let plain = render_summary(&reports, &Styling::plain()).unwrap();
        assert!(plain.starts_with("Summary: 1 error, "), "{plain}");
        assert!(plain.contains("(1 suppressed)"), "{plain}");
        assert!(plain.ends_with(" in 1 file"), "{plain}");
        assert!(render_summary(&reports, &Styling::ansi())
            .unwrap()
            .contains('\u{1b}'));
    }

    #[test]
    fn summary_counts_pluralize_at_two() {
        // Two files, each VACUUM FULL → 1 error (vacuum-full-cluster) + 1 warning
        // (require-timeout), so totals cross the singular/plural boundary.
        let reports = vec![
            lint_input("a.sql", "VACUUM FULL t;", &crate::LintOptions::default()),
            lint_input("b.sql", "VACUUM FULL t;", &crate::LintOptions::default()),
        ];
        assert_eq!(
            render_summary(&reports, &Styling::plain()).unwrap(),
            "Summary: 2 errors, 2 warnings in 2 files"
        );
    }

    #[test]
    fn summary_stands_alone_when_only_suppressed_findings_exist() {
        // DROP TABLE emits two findings (drop-table + require-timeout); stack an
        // ignore directive for each so nothing unsuppressed remains, exercising the
        // `clauses.is_empty()` branch: no comma, no parenthesis, just "N suppressed".
        let sql = "-- pgsafe:ignore drop-table cleanup\n\
                   -- pgsafe:ignore require-timeout reviewed\n\
                   DROP TABLE x;";
        let reports = vec![lint_input("m.sql", sql, &crate::LintOptions::default())];
        assert!(
            reports[0].findings.iter().all(Finding::is_suppressed),
            "fixture must have zero unsuppressed findings: {:?}",
            reports[0].findings
        );
        assert_eq!(
            render_summary(&reports, &Styling::plain()).unwrap(),
            "Summary: 2 suppressed in 1 file"
        );
    }

    #[test]
    fn color_on_follows_precedence() {
        use ColorWhen::*;
        // Never / Always ignore env + tty entirely.
        assert!(!color_on(Never, true, false, true));
        assert!(color_on(Always, false, true, false));
        // Auto: CLICOLOR_FORCE wins over NO_COLOR and a non-tty.
        assert!(color_on(Auto, true, true, false));
        // Auto: NO_COLOR (without force) is off even on a tty.
        assert!(!color_on(Auto, false, true, true));
        // Auto: neither set → follow the tty.
        assert!(color_on(Auto, false, false, true));
        assert!(!color_on(Auto, false, false, false));
    }

    #[test]
    fn resolve_always_colors_never_plain_regardless_of_env() {
        // Always/Never bypass env, so these are deterministic under any test env.
        assert!(Styling::resolve(ColorWhen::Always)
            .danger("x")
            .contains('\u{1b}'));
        assert_eq!(Styling::resolve(ColorWhen::Never).danger("x"), "x");
    }
}
