//! Cross-statement enum-value check: flag `ALTER TYPE … ADD VALUE 'v'` when `'v'` is used as a string
//! literal later in the SAME transaction. PostgreSQL forbids using a newly added enum value in the
//! transaction that added it — the later statement fails at runtime with `unsafe use of new value`
//! (SQLSTATE 55P04). This is an engine-synthesized finding, not a registered `Rule`.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::protobuf::{RawStmt, Token, TransactionStmtKind};
use crate::ast::NodeEnum;

pub(crate) const ID: &str = "enum-value-used-in-transaction";
pub(crate) const MESSAGE: &str =
    "This ALTER TYPE ... ADD VALUE adds an enum value that is used later in the same transaction. \
    PostgreSQL forbids using a newly added enum value in the transaction that added it; the later \
    statement fails at runtime with \"unsafe use of new value\".";
pub(crate) const GUIDANCE: &str =
    "Add the enum value in its own migration (or before BEGIN / outside the wrapping transaction) so \
    it is committed before any statement uses it. Many migration tools wrap each migration in an \
    implicit transaction — disable that for this migration if you must add and use the value together.";

/// The value of a string-constant (`'…'`) source slice: surrounding quotes stripped, each doubled
/// `''` collapsed to one `'`. Non-simple forms (`E'…'`, dollar-quoted) are returned as-is and simply
/// won't match a plain enum label.
fn unquote(slice: &str) -> String {
    match slice.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        Some(inner) => inner.replace("''", "'"),
        None => slice.to_string(),
    }
}

/// Each statement's `[start, end)` byte span in `sql` from `stmt_location`/`stmt_len` (`stmt_len == 0`
/// for the final statement means "to the next statement's start, or end of input").
fn statement_spans(sql: &str, stmts: &[RawStmt]) -> Vec<(usize, usize)> {
    let starts: Vec<usize> = stmts
        .iter()
        .map(|s| usize::try_from(s.stmt_location).unwrap_or(0))
        .collect();
    stmts
        .iter()
        .enumerate()
        .map(|(k, s)| {
            let start = starts[k];
            let len = usize::try_from(s.stmt_len).unwrap_or(0);
            let end = if len > 0 {
                start + len
            } else {
                starts.get(k + 1).copied().unwrap_or(sql.len())
            };
            (start, end.min(sql.len()))
        })
        .collect()
}

/// The string-literal values appearing in each statement, indexed by statement, read from the scanner
/// (`Token::Sconst`) so only real string constants match.
fn literals_by_statement(sql: &str, stmts: &[RawStmt]) -> Vec<Vec<String>> {
    let mut out = vec![Vec::new(); stmts.len()];
    let Ok(scan) = crate::ast::scan(sql) else {
        return out;
    };
    let spans = statement_spans(sql, stmts);
    for t in &scan.tokens {
        if t.token != Token::Sconst as i32 {
            continue;
        }
        let start = usize::try_from(t.start).unwrap_or(0);
        let end = usize::try_from(t.end).unwrap_or(0);
        let Some(slice) = sql.get(start..end) else {
            continue;
        };
        if let Some(k) = spans.iter().position(|&(s, e)| start >= s && start < e) {
            out[k].push(unquote(slice));
        }
    }
    out
}

/// Indices of `ALTER TYPE … ADD VALUE 'v'` statements whose value `'v'` is used as a string literal in
/// a later statement of the same transaction. `assume_in_transaction` seeds the transaction state for
/// tools that wrap each migration implicitly (mirrors `txn.rs`).
pub(crate) fn unsafe_enum_value_indices(
    sql: &str,
    stmts: &[RawStmt],
    assume_in_transaction: bool,
) -> Vec<usize> {
    let literals = literals_by_statement(sql, stmts);
    let mut in_txn = assume_in_transaction;
    let mut added: BTreeMap<String, usize> = BTreeMap::new();
    let mut flagged: BTreeSet<usize> = BTreeSet::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        if let NodeEnum::TransactionStmt(t) = node {
            match TransactionStmtKind::try_from(t.kind) {
                Ok(TransactionStmtKind::TransStmtBegin | TransactionStmtKind::TransStmtStart) => {
                    in_txn = true;
                    added.clear();
                }
                Ok(
                    TransactionStmtKind::TransStmtCommit | TransactionStmtKind::TransStmtRollback,
                ) => {
                    in_txn = false;
                    added.clear();
                }
                _ => {}
            }
            continue;
        }
        if !in_txn {
            continue;
        }
        // An enum DDL is a definition, not a use: record an ADD VALUE and move on.
        if let NodeEnum::AlterEnumStmt(s) = node {
            if s.old_val.is_empty() && !s.new_val.is_empty() {
                added.insert(s.new_val.clone(), i);
            }
            continue;
        }
        // Otherwise, does this statement use a value added earlier in the same transaction?
        for lit in &literals[i] {
            if let Some(&add_idx) = added.get(lit) {
                flagged.insert(add_idx);
            }
        }
    }
    flagged.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::unsafe_enum_value_indices;

    fn indices(sql: &str) -> Vec<usize> {
        unsafe_enum_value_indices(sql, &crate::ast::parse(sql).unwrap().protobuf.stmts, false)
    }
    fn indices_assumed(sql: &str) -> Vec<usize> {
        unsafe_enum_value_indices(sql, &crate::ast::parse(sql).unwrap().protobuf.stmts, true)
    }

    #[test]
    fn add_then_use_in_same_transaction_is_flagged() {
        assert_eq!(
            indices("BEGIN; ALTER TYPE mood ADD VALUE 'x'; INSERT INTO t VALUES ('x'); COMMIT;"),
            vec![1]
        );
    }

    #[test]
    fn add_only_in_transaction_is_not_flagged() {
        assert!(indices("BEGIN; ALTER TYPE mood ADD VALUE 'x'; COMMIT;").is_empty());
    }

    #[test]
    fn autocommit_add_then_use_is_not_flagged() {
        assert!(indices("ALTER TYPE mood ADD VALUE 'x'; INSERT INTO t VALUES ('x');").is_empty());
    }

    #[test]
    fn use_after_commit_is_not_flagged() {
        assert!(indices(
            "BEGIN; ALTER TYPE mood ADD VALUE 'x'; COMMIT; INSERT INTO t VALUES ('x');"
        )
        .is_empty());
    }

    #[test]
    fn assumed_transaction_flags_top_level_add_then_use() {
        assert_eq!(
            indices_assumed("ALTER TYPE mood ADD VALUE 'x'; INSERT INTO t VALUES ('x');"),
            vec![0]
        );
    }

    #[test]
    fn rename_value_is_not_an_add() {
        // RENAME VALUE has a non-empty old_val and is not an ADD VALUE.
        assert!(indices(
            "BEGIN; ALTER TYPE mood RENAME VALUE 'x' TO 'y'; INSERT INTO t VALUES ('x'); COMMIT;"
        )
        .is_empty());
    }

    #[test]
    fn lint_sql_emits_a_warning() {
        use crate::{lint_sql, LintOptions, Severity};
        let f = lint_sql(
            "BEGIN; ALTER TYPE mood ADD VALUE 'x'; INSERT INTO t VALUES ('x'); COMMIT;",
            &LintOptions::default(),
        )
        .unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "enum-value-used-in-transaction")
            .expect("rule must fire through the engine");
        assert_eq!(hit.severity, Severity::Warning);
    }

    #[test]
    fn finding_is_inline_suppressible() {
        use crate::{lint_sql, LintOptions};
        // The directive sits directly above the ADD VALUE statement (the flagged one).
        let sql = "BEGIN;\n-- pgsafe:ignore enum-value-used-in-transaction backfill is intended\n\
                   ALTER TYPE mood ADD VALUE 'x';\nINSERT INTO t VALUES ('x');\nCOMMIT;";
        let f = lint_sql(sql, &LintOptions::default()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "enum-value-used-in-transaction")
            .expect("rule must fire");
        assert!(hit.is_suppressed(), "directive must suppress the finding");
    }
}
