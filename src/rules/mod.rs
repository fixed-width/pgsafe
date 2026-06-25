//! Rule engine: the `Rule` trait and the registry of enabled rules.

use pg_query::NodeEnum;

use crate::RuleHit;

/// A single safety rule. Implementations inspect one statement node and push a
/// `RuleHit` for each problem they find.
pub trait Rule {
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>);
}

mod add_check_without_not_valid;
mod add_fk_without_not_valid;
mod non_concurrent_index;

/// All rules enabled in this build, in stable registration order.
pub fn all_rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(non_concurrent_index::NonConcurrentIndex),
        Box::new(add_fk_without_not_valid::AddFkWithoutNotValid),
        Box::new(add_check_without_not_valid::AddCheckWithoutNotValid),
    ]
}
