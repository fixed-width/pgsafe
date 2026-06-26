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
