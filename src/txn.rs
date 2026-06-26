//! Cross-statement transaction tracking: flag a CONCURRENTLY index operation
//! inside an explicit `BEGIN ... COMMIT` block — PostgreSQL rejects `CONCURRENTLY`
//! in a transaction, so it fails at runtime. This is an engine-synthesized
//! finding, not a registered `Rule`. Detection covers explicit transactions only.

use pg_query::protobuf::{ObjectType, RawStmt, ReindexStmt, TransactionStmtKind};
use pg_query::NodeEnum;

pub(crate) const ID: &str = "concurrently-in-transaction";
pub(crate) const MESSAGE: &str = "CREATE/DROP INDEX CONCURRENTLY and REINDEX CONCURRENTLY cannot run \
    inside a transaction block; this statement is inside an explicit BEGIN ... COMMIT and will fail \
    at runtime.";
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

/// Statement indices that are a CONCURRENTLY index operation inside an explicit
/// transaction block.
pub(crate) fn concurrently_in_transaction_indices(stmts: &[RawStmt]) -> Vec<usize> {
    let mut in_txn = false;
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
        concurrently_in_transaction_indices(&pg_query::parse(sql).unwrap().protobuf.stmts)
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
    }
}
