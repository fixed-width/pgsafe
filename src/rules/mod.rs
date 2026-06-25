//! Rule engine: the `Rule` trait and the registry of enabled rules.

use pg_query::NodeEnum;

use crate::RuleHit;

/// A single safety rule. Implementations inspect one statement node and push a
/// `RuleHit` for each problem they find.
pub trait Rule {
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>);
}

mod non_concurrent_index;

/// All rules enabled in this build, in stable registration order.
pub fn all_rules() -> Vec<Box<dyn Rule>> {
    vec![Box::new(non_concurrent_index::NonConcurrentIndex)]
}
