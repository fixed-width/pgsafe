//! Cross-statement transaction tracking: flag a CONCURRENTLY index operation
//! inside a transaction block — PostgreSQL rejects `CONCURRENTLY`
//! in a transaction, so it fails at runtime. This is an engine-synthesized
//! finding, not a registered `Rule`.

use pg_query::protobuf::{ObjectType, RawStmt, ReindexStmt, TransactionStmtKind};
use pg_query::NodeEnum;

pub(crate) const ID: &str = "concurrently-in-transaction";
pub(crate) const MESSAGE: &str =
    "CREATE/DROP INDEX CONCURRENTLY and REINDEX CONCURRENTLY cannot run \
    inside a transaction block; this statement runs inside a transaction and will fail at runtime.";
pub(crate) const GUIDANCE: &str = "Run the CONCURRENTLY statement outside the transaction — put it in \
    its own migration, or move it before BEGIN / after COMMIT. (Note: many migration tools also wrap \
    each migration in an implicit transaction; disable that for this migration.)";

/// A `REINDEX ... CONCURRENTLY` (a `concurrently` option that is true).
fn reindex_is_concurrent(r: &ReindexStmt) -> bool {
    r.params.iter().any(|p| {
        matches!(p.node.as_ref(), Some(NodeEnum::DefElem(de))
            if de.defname == "concurrently" && crate::rules::defelem_is_true(de))
    })
}

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
        NodeEnum::ReindexStmt(r) => reindex_is_concurrent(r),
        _ => false,
    }
}

/// Statement indices that are a CONCURRENTLY index operation inside a transaction block.
/// `assume_in_transaction` seeds the initial state (for tools that wrap each migration implicitly).
pub(crate) fn concurrently_in_transaction_indices(
    stmts: &[RawStmt],
    assume_in_transaction: bool,
) -> Vec<usize> {
    let mut in_txn = assume_in_transaction;
    let mut out = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        if let NodeEnum::TransactionStmt(t) = node {
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
            continue;
        }
        if in_txn && is_concurrently_index_op(node) {
            out.push(i);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indices(sql: &str) -> Vec<usize> {
        concurrently_in_transaction_indices(&pg_query::parse(sql).unwrap().protobuf.stmts, false)
    }
    fn indices_assumed(sql: &str) -> Vec<usize> {
        concurrently_in_transaction_indices(&pg_query::parse(sql).unwrap().protobuf.stmts, true)
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
