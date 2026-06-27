//! Cross-statement timeout tracking: flag a blocking-lock DDL that runs while no
//! `lock_timeout`/`statement_timeout` is in effect. A DDL that must wait for its
//! lock queues behind any in-flight query — and then blocks every query that
//! arrives behind it (a lock-queue pileup), even though the DDL itself is
//! instant. A bounded `lock_timeout` makes it fail fast instead. This is an
//! engine-synthesized finding, not a registered `Rule`.

use pg_query::protobuf::{
    a_const, Node, ObjectType, RawStmt, TransactionStmtKind, VacuumStmt, VariableSetKind,
    VariableSetStmt,
};
use pg_query::NodeEnum;

pub(crate) const ID: &str = "require-timeout";
pub(crate) const MESSAGE: &str =
    "This statement takes a lock but no lock_timeout is set — if it queues behind a slow query, \
    it blocks every query on the table until it acquires the lock.";
pub(crate) const GUIDANCE: &str =
    "Set a bounded lock_timeout first, e.g. `SET lock_timeout = '5s';` (or `SET LOCAL` inside a \
    transaction), so the statement fails fast instead of piling up the lock queue. \
    statement_timeout also satisfies this.";

/// Whether a `VACUUM` carries the `FULL` option. Plain `VACUUM` takes only
/// `SHARE UPDATE EXCLUSIVE` and does not block, so it is excluded. (Same shape
/// the `vacuum-full-cluster` rule uses.)
fn vacuum_is_full(v: &VacuumStmt) -> bool {
    v.is_vacuumcmd
        && v.options.iter().any(|opt| {
            matches!(opt.node.as_ref(), Some(NodeEnum::DefElem(de))
                if de.defname == "full" && crate::rules::defelem_is_true(de))
        })
}

/// Whether a statement must acquire a lock that conflicts with concurrent readers
/// or writers — one that can queue and pile up without a bounded timeout. The
/// CONCURRENTLY forms are deliberately excluded (they are the safe path pgsafe
/// already steers toward).
fn takes_blocking_lock(node: &NodeEnum) -> bool {
    match node {
        NodeEnum::AlterTableStmt(_) | NodeEnum::TruncateStmt(_) | NodeEnum::ClusterStmt(_) => true,
        NodeEnum::ReindexStmt(r) => !crate::rules::reindex_is_concurrent(r),
        NodeEnum::VacuumStmt(v) => vacuum_is_full(v),
        NodeEnum::IndexStmt(i) => !i.concurrent,
        NodeEnum::RefreshMatViewStmt(r) => !r.concurrent,
        NodeEnum::DropStmt(d) => match ObjectType::try_from(d.remove_type) {
            Ok(ObjectType::ObjectTable) => true,
            Ok(ObjectType::ObjectIndex) => !d.concurrent,
            _ => false,
        },
        _ => false,
    }
}

/// Whether a `SET`/`RESET` targets `lock_timeout` or `statement_timeout`.
fn is_timeout_var(name: &str) -> bool {
    name.eq_ignore_ascii_case("lock_timeout") || name.eq_ignore_ascii_case("statement_timeout")
}

/// Whether a `SET <timeout> = <value>` *activates* a timeout: it must be a
/// `VAR_SET_VALUE` (not `RESET` / `SET DEFAULT`) with a non-zero value. A literal
/// `0` / `'0'` value disables the timeout, so it does not activate.
fn set_activates(set: &VariableSetStmt) -> bool {
    if VariableSetKind::try_from(set.kind) != Ok(VariableSetKind::VarSetValue) {
        return false;
    }
    !set.args.iter().any(arg_is_zero)
}

/// Whether a `SET` argument is a literal zero (`0` or `'0'`) — the disable forms.
fn arg_is_zero(arg: &Node) -> bool {
    let Some(NodeEnum::AConst(c)) = arg.node.as_ref() else {
        return false;
    };
    match c.val.as_ref() {
        Some(a_const::Val::Ival(i)) => i.ival == 0,
        Some(a_const::Val::Sval(s)) => s.sval == "0",
        _ => false,
    }
}

/// Statement indices that take a blocking lock while no `lock_timeout` /
/// `statement_timeout` is in effect. `assume_in_transaction` seeds the
/// transaction state so a top-of-file `SET LOCAL` in a tool-wrapped migration is
/// scoped correctly (mirrors `concurrently_in_transaction_indices`).
pub(crate) fn require_timeout_indices(
    stmts: &[RawStmt],
    assume_in_transaction: bool,
) -> Vec<usize> {
    let mut in_txn = assume_in_transaction;
    let mut session_timeout = false;
    let mut local_timeout = false;
    let mut out = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        match node {
            NodeEnum::TransactionStmt(t) => match TransactionStmtKind::try_from(t.kind) {
                Ok(TransactionStmtKind::TransStmtBegin | TransactionStmtKind::TransStmtStart) => {
                    in_txn = true;
                    local_timeout = false; // a prior txn's SET LOCAL does not carry
                }
                Ok(
                    TransactionStmtKind::TransStmtCommit | TransactionStmtKind::TransStmtRollback,
                ) => {
                    in_txn = false;
                    local_timeout = false; // session_timeout persists across txns
                }
                _ => {}
            },
            NodeEnum::VariableSetStmt(set) => {
                if VariableSetKind::try_from(set.kind) == Ok(VariableSetKind::VarResetAll) {
                    // RESET ALL clears all GUCs, including any SET LOCAL timeout override.
                    session_timeout = false;
                    local_timeout = false;
                } else if is_timeout_var(&set.name) {
                    // NOTE: lock_timeout and statement_timeout are collapsed into single
                    // session_timeout/local_timeout booleans — either satisfies the rule.
                    // This means deactivating one variable while a SET LOCAL of the *other*
                    // is still active is not tracked independently. We deliberately accept
                    // this rare imprecision; tracking both vars separately is not worth it
                    // (consistent with the spec's out-of-scope notes).
                    let activates = set_activates(set);
                    if set.is_local {
                        // SET LOCAL only takes effect inside an explicit transaction.
                        if in_txn {
                            local_timeout = activates;
                        }
                    } else {
                        session_timeout = activates;
                    }
                }
            }
            _ => {
                if takes_blocking_lock(node) && !(session_timeout || local_timeout) {
                    out.push(i);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indices(sql: &str) -> Vec<usize> {
        require_timeout_indices(&pg_query::parse(sql).unwrap().protobuf.stmts, false)
    }
    fn indices_assumed(sql: &str) -> Vec<usize> {
        require_timeout_indices(&pg_query::parse(sql).unwrap().protobuf.stmts, true)
    }

    #[test]
    fn unguarded_blocking_ddl_is_flagged() {
        assert_eq!(indices("ALTER TABLE t ADD COLUMN c int;"), vec![0]);
        assert_eq!(indices("DROP TABLE t;"), vec![0]);
        assert_eq!(indices("TRUNCATE t;"), vec![0]);
        assert_eq!(indices("CLUSTER t USING i;"), vec![0]);
        assert_eq!(indices("REINDEX TABLE t;"), vec![0]);
        assert_eq!(indices("VACUUM FULL t;"), vec![0]);
        assert_eq!(indices("CREATE INDEX i ON t (x);"), vec![0]);
        assert_eq!(indices("REFRESH MATERIALIZED VIEW mv;"), vec![0]);
        assert_eq!(indices("DROP INDEX i;"), vec![0]);
    }

    #[test]
    fn safe_and_concurrent_forms_are_not_flagged() {
        assert!(indices("VACUUM t;").is_empty()); // plain VACUUM does not block
        assert!(indices("CREATE INDEX CONCURRENTLY i ON t (x);").is_empty());
        assert!(indices("REINDEX TABLE CONCURRENTLY t;").is_empty());
        assert!(indices("DROP INDEX CONCURRENTLY i;").is_empty());
        assert!(indices("REFRESH MATERIALIZED VIEW CONCURRENTLY mv;").is_empty());
        assert!(indices("SELECT 1;").is_empty());
        assert!(indices("CREATE TABLE t (id int);").is_empty()); // CREATE TABLE is not a blocking op
    }

    #[test]
    fn a_set_timeout_satisfies_following_statements() {
        assert!(indices("SET lock_timeout = '5s'; ALTER TABLE t ADD COLUMN c int;").is_empty());
        assert!(
            indices("SET statement_timeout = '5s'; ALTER TABLE t ADD COLUMN c int;").is_empty()
        );
        // ordering matters — the SET only protects later statements
        assert_eq!(
            indices("ALTER TABLE t ADD COLUMN c int; SET lock_timeout = '5s';"),
            vec![0]
        );
    }

    #[test]
    fn reset_or_zero_disables_the_timeout() {
        assert_eq!(
            indices("SET lock_timeout = '5s'; RESET lock_timeout; ALTER TABLE t ADD COLUMN c int;"),
            vec![2]
        );
        assert_eq!(
            indices(
                "SET lock_timeout = '5s'; SET lock_timeout = 0; ALTER TABLE t ADD COLUMN c int;"
            ),
            vec![2]
        );
        assert_eq!(
            indices("SET lock_timeout = '5s'; RESET ALL; ALTER TABLE t ADD COLUMN c int;"),
            vec![2]
        );
        // string-zero '0' also disables (arg_is_zero handles Sval "0")
        assert_eq!(
            indices(
                "SET lock_timeout = '5s'; SET lock_timeout = '0'; ALTER TABLE t ADD COLUMN c int;"
            ),
            vec![2]
        );
        // SET ... = DEFAULT deactivates (VarSetDefault is not VarSetValue)
        assert_eq!(
            indices(
                "SET lock_timeout = '5s'; SET lock_timeout = DEFAULT; ALTER TABLE t ADD COLUMN c int;"
            ),
            vec![2]
        );
    }

    #[test]
    fn reset_all_clears_a_set_local_timeout() {
        // RESET ALL inside the txn disables the SET LOCAL timeout for the rest of the txn,
        // so the following ALTER is unguarded and flagged.
        assert_eq!(
            indices(
                "BEGIN; SET LOCAL lock_timeout = '5s'; RESET ALL; ALTER TABLE t ADD COLUMN c int; COMMIT;"
            ),
            vec![3]
        );
    }

    #[test]
    fn set_local_is_scoped_to_its_transaction() {
        // inside the transaction it satisfies
        assert!(indices(
            "BEGIN; SET LOCAL lock_timeout = '5s'; ALTER TABLE t ADD COLUMN c int; COMMIT;"
        )
        .is_empty());
        // after COMMIT the SET LOCAL has expired
        assert_eq!(
            indices(
                "BEGIN; SET LOCAL lock_timeout = '5s'; COMMIT; ALTER TABLE t ADD COLUMN c int;"
            ),
            vec![3]
        );
        // SET LOCAL at top level (no BEGIN) is a no-op — does not satisfy
        assert_eq!(
            indices("SET LOCAL lock_timeout = '5s'; ALTER TABLE t ADD COLUMN c int;"),
            vec![1]
        );
    }

    #[test]
    fn assume_in_transaction_scopes_a_top_of_file_set_local() {
        // With the wrapping-transaction assumption, a top-of-file SET LOCAL is in scope.
        assert!(
            indices_assumed("SET LOCAL lock_timeout = '5s'; ALTER TABLE t ADD COLUMN c int;")
                .is_empty()
        );
    }

    #[test]
    fn each_unguarded_statement_is_flagged_once() {
        assert_eq!(
            indices("ALTER TABLE a ADD COLUMN c int; ALTER TABLE b ADD COLUMN c int;"),
            vec![0, 1]
        );
    }

    #[test]
    fn lint_sql_emits_a_require_timeout_warning() {
        use crate::{lint_sql, LintOptions, Severity};
        let fs = lint_sql("ALTER TABLE t ADD COLUMN c int;", &LintOptions::default()).unwrap();
        let f = fs.iter().find(|f| f.rule_id == ID).unwrap();
        assert_eq!(f.severity, Severity::Warning);
    }

    #[test]
    fn new_empty_table_op_is_exempt() {
        use crate::{lint_sql, LintOptions};
        // The ALTER targets a table created empty in the same input → no require-timeout.
        let fs = lint_sql(
            "CREATE TABLE foo (id int); ALTER TABLE foo ADD COLUMN c int;",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(!fs.iter().any(|f| f.rule_id == ID));
    }
}
