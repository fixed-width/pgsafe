use crate::ast::protobuf::{ConstrType, Node};
use crate::ast::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

/// Known-volatile functions whose use in an `ADD COLUMN` default forces a full
/// table rewrite. Stable functions (`now`, `current_timestamp`, …) are excluded
/// because PostgreSQL evaluates them once and keeps the fast path.
const VOLATILE_FUNCTIONS: &[&str] = &[
    "random",
    "random_normal",
    "gen_random_uuid",
    "gen_random_bytes",
    "uuid_generate_v4",
    "uuid_generate_v1",
    "uuid_generate_v1mc",
    "clock_timestamp",
    "timeofday",
    "nextval",
];

pub struct AddColumnVolatileDefault;

impl Rule for AddColumnVolatileDefault {
    fn id(&self) -> &'static str {
        "add-column-volatile-default"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for col in super::columns_being_added(node) {
            let has_volatile_default = col.constraints.iter().any(|cn| {
                let Some(NodeEnum::Constraint(con)) = cn.node.as_ref() else {
                    return false;
                };
                matches!(
                    ConstrType::try_from(con.contype),
                    Ok(ConstrType::ConstrDefault)
                ) && con
                    .raw_expr
                    .as_deref()
                    .and_then(|n| n.node.as_ref())
                    .is_some_and(expr_contains_volatile)
            });
            if has_volatile_default {
                out.push(RuleHit {
                    message: "ADD COLUMN with a volatile DEFAULT (e.g. random(), gen_random_uuid()) \
                              rewrites every existing row under an ACCESS EXCLUSIVE lock."
                        .into(),
                    guidance: "Add the column nullable with no default, backfill existing rows in \
                               batches, then ALTER COLUMN ... SET DEFAULT for new rows (add NOT NULL \
                               via the safe two-step if needed)."
                        .into(),
                    fix: None,
                });
            }
        }
    }
}

/// Whether an expression subtree calls a denylisted volatile function. Recurses
/// through the expression-composition nodes that realistically appear in a
/// default; any other node type is treated as a leaf.
fn expr_contains_volatile(node: &NodeEnum) -> bool {
    match node {
        NodeEnum::FuncCall(fc) => {
            is_volatile_funcname(&fc.funcname) || fc.args.iter().any(node_contains_volatile)
        }
        NodeEnum::TypeCast(tc) => tc.arg.as_deref().is_some_and(node_contains_volatile),
        NodeEnum::AExpr(e) => {
            e.lexpr.as_deref().is_some_and(node_contains_volatile)
                || e.rexpr.as_deref().is_some_and(node_contains_volatile)
        }
        NodeEnum::BoolExpr(e) => e.args.iter().any(node_contains_volatile),
        NodeEnum::CoalesceExpr(e) => e.args.iter().any(node_contains_volatile),
        NodeEnum::CaseExpr(e) => {
            e.arg.as_deref().is_some_and(node_contains_volatile)
                || e.defresult.as_deref().is_some_and(node_contains_volatile)
                || e.args.iter().any(node_contains_volatile)
        }
        NodeEnum::CaseWhen(w) => {
            w.expr.as_deref().is_some_and(node_contains_volatile)
                || w.result.as_deref().is_some_and(node_contains_volatile)
        }
        NodeEnum::MinMaxExpr(e) => e.args.iter().any(node_contains_volatile),
        NodeEnum::AArrayExpr(e) => e.elements.iter().any(node_contains_volatile),
        _ => false,
    }
}

/// Unwrap a `Node` and test its inner expression.
fn node_contains_volatile(n: &Node) -> bool {
    n.node.as_ref().is_some_and(expr_contains_volatile)
}

/// Whether the last element of a (possibly schema-qualified) function name is on
/// the volatile denylist, matched case-insensitively.
fn is_volatile_funcname(funcname: &[Node]) -> bool {
    match funcname.last().and_then(|n| n.node.as_ref()) {
        Some(NodeEnum::String(s)) => VOLATILE_FUNCTIONS
            .iter()
            .any(|f| f.eq_ignore_ascii_case(&s.sval)),
        _ => false,
    }
}
