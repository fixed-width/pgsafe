//! `source.fixAll` — apply every auto-fix at once, with `pgsafe --fix` semantics.

use std::collections::HashMap;

use lsp_types::{CodeAction, CodeActionKind, CodeActionOrCommand, TextEdit, Uri, WorkspaceEdit};

use super::position::LineIndex;
use crate::fix::{fix_to_fixpoint, MAX_FIX_ITERATIONS};
use crate::{lint_input, LintOptions};

/// Build the `source.fixAll` action for `sql`: drive the same fixpoint engine
/// `pgsafe --fix` uses (iterate to a safe fixpoint — parse-valid, introducing no new
/// Error), and if that changes the text, return a single whole-document replacement.
/// `None` when nothing is safely auto-fixable (the fixpoint made no change).
// `Uri` as a `HashMap` key is hash-stable — see the rationale on `actions::code_actions`.
#[allow(clippy::mutable_key_type)]
pub(crate) fn fix_all_action(
    uri: &Uri,
    sql: &str,
    options: &LintOptions,
) -> Option<CodeActionOrCommand> {
    // Re-lint each candidate with the caller's options, exactly as `--fix` does. The
    // fixpoint only consults the report's findings/error, so the display name is
    // immaterial.
    let fixed = fix_to_fixpoint(
        sql,
        |candidate| lint_input(uri.as_str(), candidate, options),
        MAX_FIX_ITERATIONS,
    );
    // `fixed.sql` only advances past `sql` when at least one pass was accepted, and
    // every accepted state is validated (parses, introduces no new Error). So an
    // unchanged result means nothing was safely auto-fixable.
    if fixed.sql == sql {
        return None;
    }
    let whole = LineIndex::new(sql).range(0, sql.len());
    let mut changes = HashMap::new();
    changes.insert(
        uri.clone(),
        vec![TextEdit {
            range: whole,
            new_text: fixed.sql,
        }],
    );
    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: "pgsafe: fix all auto-fixable findings".to_string(),
        kind: Some(CodeActionKind::SOURCE_FIX_ALL),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..WorkspaceEdit::default()
        }),
        ..CodeAction::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::fix_all_action;
    use crate::fix::{fix_to_fixpoint, MAX_FIX_ITERATIONS};
    use crate::{lint_input, LintOptions};
    use lsp_types::{CodeActionKind, CodeActionOrCommand, Range, Uri};

    fn uri() -> Uri {
        "file:///tmp/a.sql".parse().unwrap()
    }

    /// Unwrap the single whole-document `TextEdit` from a `source.fixAll` action,
    /// asserting the action's shape along the way.
    #[allow(clippy::mutable_key_type)]
    fn edit(action: &CodeActionOrCommand) -> (Range, String) {
        let ca = match action {
            CodeActionOrCommand::CodeAction(ca) => ca,
            CodeActionOrCommand::Command(_) => panic!("expected a code action, not a command"),
        };
        assert_eq!(ca.kind, Some(CodeActionKind::SOURCE_FIX_ALL));
        let changes = ca.edit.as_ref().unwrap().changes.as_ref().unwrap();
        let edits = &changes[&uri()];
        assert_eq!(edits.len(), 1, "fixAll is a single whole-doc replacement");
        (edits[0].range, edits[0].new_text.clone())
    }

    #[test]
    #[allow(clippy::mutable_key_type)]
    fn fixes_all_findings_as_one_whole_doc_edit() {
        let sql = "CREATE INDEX idx ON t (col);";
        let opts = LintOptions::default();
        let action = fix_all_action(&uri(), sql, &opts).expect("fixable input → an action");
        let (range, text) = edit(&action);
        // Whole-document replacement: [ (0,0) .. (0, len) ) over the ASCII fixture.
        assert_eq!((range.start.line, range.start.character), (0, 0));
        assert_eq!(
            (range.end.line, range.end.character),
            (0, u32::try_from(sql.len()).unwrap())
        );
        // The replacement is byte-identical to what `pgsafe --fix` produces (same engine).
        let expected = fix_to_fixpoint(sql, |s| lint_input("x", s, &opts), MAX_FIX_ITERATIONS).sql;
        assert_eq!(text, expected);
        assert_ne!(text, sql, "the fix must change the text");
        assert!(
            text.contains("CONCURRENTLY"),
            "the add-index fix should apply"
        );
    }

    #[test]
    #[allow(clippy::mutable_key_type)]
    fn nothing_fixable_yields_no_action() {
        // Clean SQL: no findings, so the fixpoint is a no-op → no action.
        let opts = LintOptions::default();
        assert!(fix_all_action(&uri(), "SELECT 1;", &opts).is_none());
    }

    #[test]
    #[allow(clippy::mutable_key_type)]
    fn whole_doc_range_spans_a_multiline_input() {
        // Two statements, no trailing newline: the replacement range must cover to the
        // end of the second line regardless of where the fixes land.
        let sql = "DROP TABLE a;\nCREATE INDEX idx ON t (col);";
        let opts = LintOptions::default();
        let action = fix_all_action(&uri(), sql, &opts).expect("fixable");
        let (range, _text) = edit(&action);
        assert_eq!((range.start.line, range.start.character), (0, 0));
        assert_eq!(
            (range.end.line, range.end.character),
            (
                1,
                u32::try_from("CREATE INDEX idx ON t (col);".len()).unwrap()
            )
        );
    }
}
