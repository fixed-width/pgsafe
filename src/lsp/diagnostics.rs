//! `Finding` → LSP `Diagnostic` translation.

use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString};

use crate::{Finding, Severity};

use super::position::LineIndex;

/// Translate the findings for `sql` into LSP diagnostics, skipping suppressed ones.
pub(crate) fn diagnostics_for(sql: &str, findings: &[Finding]) -> Vec<Diagnostic> {
    let index = LineIndex::new(sql);
    findings
        .iter()
        .filter(|f| !f.is_suppressed())
        .map(|f| finding_diagnostic(f, &index))
        .collect()
}

/// Build the single `Diagnostic` for `finding`. Shared with `actions::code_actions`,
/// which links this same diagnostic to the quickfix it offers.
pub(crate) fn finding_diagnostic(finding: &Finding, index: &LineIndex) -> Diagnostic {
    let start = finding.location.byte as usize;
    let end = start + finding.snippet.len();
    Diagnostic {
        range: index.range(start, end),
        severity: Some(match finding.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
        }),
        code: Some(NumberOrString::String(finding.rule_id.clone())),
        source: Some("pgsafe".to_string()),
        message: format!("{}\n{}", finding.message, finding.guidance),
        ..Diagnostic::default()
    }
}

#[cfg(test)]
mod tests {
    use super::diagnostics_for;
    use crate::{lint_sql, LintOptions};
    use lsp_types::{DiagnosticSeverity, NumberOrString};

    #[test]
    fn maps_a_finding_to_a_diagnostic() {
        let sql = "CREATE INDEX idx ON t (col);";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        assert!(!findings.is_empty(), "fixture should trigger a rule");
        let diags = diagnostics_for(sql, &findings);
        assert_eq!(diags.len(), findings.len());
        let d = &diags[0];
        assert_eq!(d.source.as_deref(), Some("pgsafe"));
        assert_eq!(
            d.code,
            Some(NumberOrString::String(findings[0].rule_id.clone()))
        );
        assert!(
            d.severity == Some(DiagnosticSeverity::WARNING)
                || d.severity == Some(DiagnosticSeverity::ERROR)
        );
        assert!(d.message.contains(&findings[0].message));
        assert!(d.message.contains(&findings[0].guidance));
        // Range starts at the statement's first token (byte 0 here).
        assert_eq!((d.range.start.line, d.range.start.character), (0, 0));
        assert!(d.range.end.character > 0);
    }

    #[test]
    fn suppressed_findings_are_not_emitted() {
        let sql = "CREATE INDEX idx ON t (col); -- pgsafe:ignore add-index-non-concurrent reason";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        assert!(
            findings.iter().any(|f| f.is_suppressed()),
            "fixture should produce a suppressed finding"
        );
        let diags = diagnostics_for(sql, &findings);
        assert_eq!(
            diags.len(),
            findings.iter().filter(|f| !f.is_suppressed()).count()
        );
    }
}
