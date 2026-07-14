//! `Fix` → quickfix `CodeAction` translation.

use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, Range, TextEdit, Uri, WorkspaceEdit,
};

use super::diagnostics::finding_diagnostic;
use super::position::LineIndex;
use crate::Finding;

/// Build quickfix actions for every fixable finding whose statement range
/// intersects `requested`.
// `lsp_types::Uri` wraps `fluent_uri::Uri`, which caches a parsed offset in a
// `Cell` — clippy sees that interior mutability and flags `HashMap<Uri, _>` as a
// mutable-key hazard. `Uri`'s `Hash`/`Eq` impls only ever consult `as_str()`
// (see lsp-types' uri.rs), so the cache can never desync a key's hash bucket;
// the required `WorkspaceEdit.changes` shape leaves no way to avoid the type.
#[allow(clippy::mutable_key_type)]
pub(crate) fn code_actions(
    uri: &Uri,
    sql: &str,
    findings: &[Finding],
    requested: Range,
) -> Vec<CodeActionOrCommand> {
    let index = LineIndex::new(sql);
    let mut actions = Vec::new();
    for finding in findings.iter().filter(|f| !f.is_suppressed()) {
        let Some(fix) = &finding.fix else { continue };
        let start = finding.location.byte as usize;
        let end = start + finding.snippet.len();
        let finding_range = index.range(start, end);
        if !ranges_intersect(finding_range, requested) {
            continue;
        }
        let edits: Vec<TextEdit> = fix
            .edits
            .iter()
            .map(|e| TextEdit {
                range: index.range(e.start as usize, e.end as usize),
                new_text: e.replacement.clone(),
            })
            .collect();
        let mut changes = HashMap::new();
        changes.insert(uri.clone(), edits);
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: fix.title.clone(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![finding_diagnostic(finding, &index)]),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                ..WorkspaceEdit::default()
            }),
            ..CodeAction::default()
        }));
    }
    actions
}

/// Range intersection in LSP position space. Ranges that merely *touch* — one's end
/// equal to the other's start — count as intersecting, because `before` uses strict
/// `<`. This is deliberate: a cursor placed exactly at a statement boundary still gets
/// that statement's quickfix (matching VS Code's `Range.intersection`, which returns a
/// zero-length intersection rather than nothing when ranges touch). It can only add a
/// convenience action at a boundary, never drop a legitimate one.
fn ranges_intersect(a: Range, b: Range) -> bool {
    !(before(a.end, b.start) || before(b.end, a.start))
}

/// `p` is strictly before `q`.
fn before(p: lsp_types::Position, q: lsp_types::Position) -> bool {
    (p.line, p.character) < (q.line, q.character)
}

#[cfg(test)]
mod tests {
    use super::code_actions;
    use crate::{lint_sql, LintOptions};
    use lsp_types::{CodeActionOrCommand, Position, Range, Uri};

    fn uri() -> Uri {
        "file:///tmp/a.sql".parse().unwrap()
    }

    #[test]
    // See the rationale on `code_actions` for why `Uri` as a map key is safe here.
    #[allow(clippy::mutable_key_type)]
    fn fixable_finding_yields_a_quickfix() {
        // A CREATE INDEX without CONCURRENTLY has a machine-applicable fix.
        let sql = "CREATE INDEX idx ON t (col);";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        assert!(findings.iter().any(|f| f.fix.is_some()));
        let whole = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: u32::try_from(sql.len()).unwrap(),
            },
        };
        let actions = code_actions(&uri(), sql, &findings, whole);
        let quickfix = actions.iter().find_map(|a| match a {
            CodeActionOrCommand::CodeAction(ca) => Some(ca),
            CodeActionOrCommand::Command(_) => None,
        });
        let ca = quickfix.expect("expected a quickfix code action");
        assert!(ca.edit.is_some());
        let edit = ca.edit.as_ref().unwrap();
        let changes = edit.changes.as_ref().unwrap();
        assert!(changes.contains_key(&uri()));
        assert!(!changes[&uri()].is_empty());
    }

    #[test]
    fn out_of_range_request_yields_nothing() {
        let sql = "CREATE INDEX idx ON t (col);";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        // A request far past the statement.
        let far = Range {
            start: Position {
                line: 99,
                character: 0,
            },
            end: Position {
                line: 99,
                character: 1,
            },
        };
        assert!(code_actions(&uri(), sql, &findings, far).is_empty());
    }

    #[test]
    fn touching_ranges_intersect_but_gapped_ranges_do_not() {
        use super::ranges_intersect;
        let r = |sl, sc, el, ec| Range {
            start: Position {
                line: sl,
                character: sc,
            },
            end: Position {
                line: el,
                character: ec,
            },
        };
        // Touching at a single point (a.end == b.start) counts as intersecting, in
        // either order — a cursor exactly at a statement boundary still gets its fix.
        assert!(ranges_intersect(r(0, 0, 0, 10), r(0, 10, 0, 20)));
        assert!(ranges_intersect(r(0, 10, 0, 20), r(0, 0, 0, 10)));
        // A genuine gap between them does not intersect.
        assert!(!ranges_intersect(r(0, 0, 0, 5), r(0, 6, 0, 9)));
        // Overlap intersects.
        assert!(ranges_intersect(r(0, 0, 0, 10), r(0, 5, 0, 15)));
    }
}
