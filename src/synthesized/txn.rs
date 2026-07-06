//! Cross-statement transaction tracking: flag a CONCURRENTLY index operation
//! inside a transaction block — PostgreSQL rejects `CONCURRENTLY`
//! in a transaction, so it fails at runtime. This is an engine-synthesized
//! finding, not a registered `Rule`.

use crate::ast::protobuf::{AlterTableType, ObjectType, RawStmt, TransactionStmtKind};
use crate::ast::NodeEnum;
use crate::Finding;

pub(crate) const ID: &str = "concurrently-in-transaction";
pub(crate) const MESSAGE: &str =
    "CREATE/DROP INDEX CONCURRENTLY and REINDEX CONCURRENTLY cannot run \
    inside a transaction block; this statement runs inside a transaction and will fail at runtime.";
pub(crate) const GUIDANCE: &str = "Run the CONCURRENTLY statement outside the transaction — put it in \
    its own migration, or move it before BEGIN / after COMMIT. (Note: many migration tools also wrap \
    each migration in an implicit transaction; disable that for this migration.)";

/// Whether a statement is a CONCURRENTLY index operation.
fn is_concurrently_index_op(node: &NodeEnum) -> bool {
    match node {
        NodeEnum::IndexStmt(i) => i.concurrent,
        NodeEnum::DropStmt(d) => {
            d.concurrent
                && matches!(
                    ObjectType::try_from(d.remove_type),
                    Ok(ObjectType::ObjectIndex)
                )
        }
        NodeEnum::ReindexStmt(r) => crate::rules::reindex_is_concurrent(r),
        // ALTER TABLE ... DETACH PARTITION ... CONCURRENTLY is also rejected inside a
        // transaction block (the CONCURRENTLY flag lives on the PartitionCmd).
        NodeEnum::AlterTableStmt(_) => crate::rules::alter_table_cmds(node).iter().any(|cmd| {
            AlterTableType::try_from(cmd.subtype) == Ok(AlterTableType::AtDetachPartition)
                && matches!(
                    cmd.def.as_ref().and_then(|n| n.node.as_ref()),
                    Some(NodeEnum::PartitionCmd(pc)) if pc.concurrent
                )
        }),
        _ => false,
    }
}

/// Per-statement flag: `out[i]` is true when statement `i` executes inside an open
/// transaction block. `BEGIN`/`START TRANSACTION` opens the block; `COMMIT`/`ROLLBACK`
/// closes it. `assume_in_transaction` seeds the initial state (migration tools that wrap
/// each file in an implicit transaction). The flag recorded for a transaction-control
/// statement is its pre-execution state; callers key on non-control statements, so that
/// choice does not matter. The result is index-aligned with `stmts`.
pub(crate) fn in_transaction_flags(stmts: &[RawStmt], assume_in_transaction: bool) -> Vec<bool> {
    let mut in_txn = assume_in_transaction;
    let mut out = Vec::with_capacity(stmts.len());
    for raw in stmts {
        out.push(in_txn);
        if let Some(NodeEnum::TransactionStmt(t)) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) {
            match TransactionStmtKind::try_from(t.kind) {
                Ok(TransactionStmtKind::TransStmtBegin | TransactionStmtKind::TransStmtStart) => {
                    in_txn = true;
                }
                Ok(
                    TransactionStmtKind::TransStmtCommit | TransactionStmtKind::TransStmtRollback,
                ) => {
                    in_txn = false;
                }
                _ => {}
            }
        }
    }
    out
}

/// Statement indices that are a CONCURRENTLY index operation inside a transaction block.
/// `assume_in_transaction` seeds the initial state (for tools that wrap each migration implicitly).
pub(crate) fn concurrently_in_transaction_indices(
    stmts: &[RawStmt],
    assume_in_transaction: bool,
) -> Vec<usize> {
    let in_txn = in_transaction_flags(stmts, assume_in_transaction);
    stmts
        .iter()
        .enumerate()
        .filter_map(|(i, raw)| {
            let node = raw.stmt.as_ref().and_then(|b| b.node.as_ref())?;
            (in_txn[i] && is_concurrently_index_op(node)).then_some(i)
        })
        .collect()
}

/// Rules whose autofix inserts CONCURRENTLY, which PostgreSQL rejects inside a
/// transaction block. `REFRESH MATERIALIZED VIEW CONCURRENTLY` is txn-legal and
/// carries no autofix, so it is deliberately excluded.
const CONCURRENTLY_FIX_RULES: &[&str] = &[
    "add-index-non-concurrent",
    "drop-index-non-concurrent",
    "reindex-non-concurrent",
    "detach-partition-non-concurrent",
];

/// Withdraw the CONCURRENTLY autofix from any finding whose statement executes inside a
/// transaction block: `CREATE`/`DROP INDEX CONCURRENTLY`, `REINDEX … CONCURRENTLY`, and
/// `DETACH PARTITION … CONCURRENTLY` all fail at runtime inside a txn, so applying the fix
/// would swap a lint Error for a latent runtime failure. The finding is left intact (its
/// guidance already tells the user to move the statement to its own migration); only `fix`
/// is cleared.
pub(crate) fn suppress_concurrently_fix_in_transaction(
    stmts: &[RawStmt],
    findings: &mut [Finding],
    assume_in_transaction: bool,
) {
    let in_txn = in_transaction_flags(stmts, assume_in_transaction);
    for f in findings.iter_mut() {
        if f.fix.is_some()
            && CONCURRENTLY_FIX_RULES.contains(&f.rule_id.as_str())
            && in_txn.get(f.statement_index).copied().unwrap_or(false)
        {
            f.fix = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indices(sql: &str) -> Vec<usize> {
        concurrently_in_transaction_indices(&crate::ast::parse(sql).unwrap().protobuf.stmts, false)
    }
    fn indices_assumed(sql: &str) -> Vec<usize> {
        concurrently_in_transaction_indices(&crate::ast::parse(sql).unwrap().protobuf.stmts, true)
    }

    #[test]
    fn detector() {
        assert_eq!(
            indices("BEGIN; CREATE INDEX CONCURRENTLY i ON t (x); COMMIT;"),
            vec![1]
        );
        assert_eq!(
            indices("BEGIN; DROP INDEX CONCURRENTLY i; COMMIT;"),
            vec![1]
        );
        assert_eq!(
            indices("START TRANSACTION; REINDEX INDEX CONCURRENTLY i; COMMIT;"),
            vec![1]
        );
        assert!(indices("CREATE INDEX CONCURRENTLY i ON t (x);").is_empty());
        assert!(indices("BEGIN; COMMIT; CREATE INDEX CONCURRENTLY i ON t (x);").is_empty());
        assert!(indices("BEGIN; CREATE INDEX i ON t (x); COMMIT;").is_empty());
        // ROLLBACK exits the transaction
        assert!(indices("BEGIN; ROLLBACK; CREATE INDEX CONCURRENTLY i ON t (x);").is_empty());
        // an op before ROLLBACK was still inside the transaction
        assert_eq!(
            indices("BEGIN; CREATE INDEX CONCURRENTLY i ON t (x); ROLLBACK;"),
            vec![1]
        );
    }

    #[test]
    fn detects_detach_partition_concurrently_in_transaction() {
        assert_eq!(
            indices("BEGIN; ALTER TABLE p DETACH PARTITION p1 CONCURRENTLY; COMMIT;"),
            vec![1]
        );
        // A concurrent DETACH outside a txn is not flagged by this detector.
        assert!(indices("ALTER TABLE p DETACH PARTITION p1 CONCURRENTLY;").is_empty());
        // A non-concurrent DETACH is not this detector's concern (the rule handles it).
        assert!(indices("BEGIN; ALTER TABLE p DETACH PARTITION p1; COMMIT;").is_empty());
    }

    #[test]
    fn in_transaction_flags_track_block_boundaries() {
        let flags = |sql: &str| {
            super::in_transaction_flags(&crate::ast::parse(sql).unwrap().protobuf.stmts, false)
        };
        // BEGIN itself is recorded pre-open (false); the op after it is inside (true);
        // COMMIT and everything after are outside again.
        assert_eq!(
            flags("BEGIN; CREATE INDEX i ON t (x); COMMIT; SELECT 1;"),
            vec![false, true, true, false]
        );
        // No transaction control: every statement is top-level.
        assert_eq!(flags("SELECT 1; SELECT 2;"), vec![false, false]);
        // assume_in_transaction seeds the initial state.
        let seeded = super::in_transaction_flags(
            &crate::ast::parse("SELECT 1;").unwrap().protobuf.stmts,
            true,
        );
        assert_eq!(seeded, vec![true]);
    }

    #[test]
    fn assume_in_transaction_flags_top_level_concurrently() {
        // Top-level CONCURRENTLY is flagged when we assume a wrapping transaction…
        assert_eq!(
            indices_assumed("CREATE INDEX CONCURRENTLY i ON t (x);"),
            vec![0]
        );
        // …but an explicit COMMIT exits the assumed transaction.
        assert!(indices_assumed("COMMIT; CREATE INDEX CONCURRENTLY i ON t (x);").is_empty());
        // An explicit BEGIN … COMMIT is still flagged with the flag on.
        assert_eq!(
            indices_assumed("BEGIN; CREATE INDEX CONCURRENTLY i ON t (x); COMMIT;"),
            vec![1]
        );
        // Default (off) is unchanged: top-level is not flagged.
        assert!(indices("CREATE INDEX CONCURRENTLY i ON t (x);").is_empty());
    }
}
