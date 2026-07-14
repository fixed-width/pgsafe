//! `textDocument/hover` — render the pgsafe finding(s) under the cursor.

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position, Range};

use super::position::LineIndex;
use crate::Finding;

/// Build the hover for `position` in `sql`: the non-suppressed findings whose
/// statement range covers the cursor, rendered as Markdown. `None` when nothing
/// applies there (the server answers that as a JSON `null` — "no hover").
pub(crate) fn hover(sql: &str, findings: &[Finding], position: Position) -> Option<Hover> {
    let index = LineIndex::new(sql);
    let mut sections: Vec<String> = Vec::new();
    let mut range: Option<Range> = None;
    for finding in findings.iter().filter(|f| !f.is_suppressed()) {
        let start = finding.location.byte as usize;
        let end = start + finding.snippet.len();
        let finding_range = index.range(start, end);
        if !contains(finding_range, position) {
            continue;
        }
        // All overlapping findings share the same statement span (each anchors at the
        // statement's first token), so the first match's range marks the whole span.
        range.get_or_insert(finding_range);
        sections.push(format!(
            "**{} · {}**\n\n{}\n\n{}",
            finding.severity, finding.rule_id, finding.message, finding.guidance
        ));
    }
    if sections.is_empty() {
        return None;
    }
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: sections.join("\n\n---\n\n"),
        }),
        range,
    })
}

/// Whether `position` falls within the half-open LSP range `[start, end)` — the
/// statement's characters, end-exclusive (a cursor just past the last character
/// isn't "on" the statement).
fn contains(range: Range, position: Position) -> bool {
    let p = (position.line, position.character);
    (range.start.line, range.start.character) <= p && p < (range.end.line, range.end.character)
}

#[cfg(test)]
mod tests {
    use super::hover;
    use crate::{lint_sql, LintOptions};
    use lsp_types::Position;

    fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn hovering_a_flagged_statement_shows_its_finding() {
        let sql = "CREATE INDEX idx ON t (col);";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        let f = &findings[0];
        let h = hover(sql, &findings, at(0, 3)).expect("cursor inside the statement → a hover");
        let text = match h.contents {
            lsp_types::HoverContents::Markup(m) => m.value,
            other => panic!("expected markup contents, got {other:?}"),
        };
        assert!(text.contains(&f.rule_id), "hover should name the rule id");
        assert!(text.contains(&f.message), "hover should carry the message");
        assert!(
            text.contains(&f.guidance),
            "hover should carry the guidance"
        );
        assert!(text.contains("error"), "hover should show the severity");
    }

    #[test]
    fn hovering_off_any_finding_yields_none() {
        // A clean statement produces no findings, so any position yields no hover.
        let sql = "CREATE INDEX CONCURRENTLY idx ON t (col);";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        assert!(hover(sql, &findings, at(0, 3)).is_none());
    }

    #[test]
    fn hovering_before_the_statement_range_yields_none() {
        // Two statements: line 0 sets lock_timeout (clean, and satisfies require-timeout
        // so it doesn't anchor a synthesized finding at the top), line 1 drops a table.
        // A cursor on line 0 must not surface line 1's drop-table hover.
        let sql = "SET lock_timeout = '5s';\nDROP TABLE t;";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        assert!(
            findings.iter().any(|f| f.rule_id == "drop-table"),
            "fixture should flag drop-table"
        );
        assert!(
            hover(sql, &findings, at(0, 4)).is_none(),
            "hovering the clean SET line must not surface the drop-table hover"
        );
        assert!(
            hover(sql, &findings, at(1, 2)).is_some(),
            "hovering the DROP TABLE statement should surface its hover"
        );
    }

    #[test]
    fn multiple_findings_on_one_statement_are_all_shown() {
        // A CREATE INDEX (non-concurrent) with no lock_timeout set fires both
        // add-index-non-concurrent and require-timeout on the same statement.
        let sql = "CREATE INDEX idx ON t (col);";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        assert!(
            findings.len() >= 2,
            "fixture should raise at least two findings, got {}",
            findings.len()
        );
        let h = hover(sql, &findings, at(0, 3)).unwrap();
        let text = match h.contents {
            lsp_types::HoverContents::Markup(m) => m.value,
            other => panic!("expected markup, got {other:?}"),
        };
        for f in &findings {
            assert!(
                text.contains(&f.rule_id),
                "every finding's rule id should appear; missing {}",
                f.rule_id
            );
        }
    }

    #[test]
    fn suppressed_findings_do_not_hover() {
        // add-index-non-concurrent is suppressed inline; require-timeout still fires
        // unsuppressed on the same statement. The hover must show require-timeout and
        // omit the suppressed rule — proving suppression filters the hover, not that
        // the whole hover vanishes.
        let sql = "CREATE INDEX idx ON t (col); -- pgsafe:ignore add-index-non-concurrent reason";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        assert!(
            findings.iter().any(|f| f.is_suppressed()),
            "fixture should suppress a finding"
        );
        let h = hover(sql, &findings, at(0, 3))
            .expect("require-timeout still fires unsuppressed → a hover");
        let text = match h.contents {
            lsp_types::HoverContents::Markup(m) => m.value,
            other => panic!("expected markup, got {other:?}"),
        };
        assert!(
            text.contains("require-timeout"),
            "the unsuppressed require-timeout finding should hover, got: {text}"
        );
        assert!(
            !text.contains("add-index-non-concurrent"),
            "the suppressed rule must not appear in the hover"
        );
    }

    #[test]
    fn hover_range_marks_the_statement_span() {
        let sql = "CREATE INDEX idx ON t (col);";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        let h = hover(sql, &findings, at(0, 3)).unwrap();
        let r = h.range.expect("hover should carry the statement range");
        assert_eq!((r.start.line, r.start.character), (0, 0));
        assert!(r.end.character > r.start.character);
    }
}
