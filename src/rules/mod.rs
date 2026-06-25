//! Rule engine: the `Rule` trait and the registry of enabled rules.

use std::sync::LazyLock;

use pg_query::NodeEnum;

use crate::{RuleHit, Severity};

/// A single safety rule.
pub trait Rule: Send + Sync {
    /// Stable kebab-case id — the public contract key, unique across the registry.
    fn id(&self) -> &'static str;
    /// Severity for every hit this rule emits.
    fn severity(&self) -> Severity {
        Severity::Warning
    }
    /// Optional documentation URL for this rule.
    fn docs_url(&self) -> Option<&'static str> {
        None
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>);
}

mod add_check_without_not_valid;
mod add_fk_without_not_valid;
mod alter_column_type;
mod non_concurrent_index;
mod rename;
mod set_not_null;

static RULES: LazyLock<Vec<Box<dyn Rule>>> = LazyLock::new(|| {
    vec![
        Box::new(non_concurrent_index::NonConcurrentIndex),
        Box::new(add_fk_without_not_valid::AddFkWithoutNotValid),
        Box::new(add_check_without_not_valid::AddCheckWithoutNotValid),
        Box::new(set_not_null::SetNotNull),
        Box::new(alter_column_type::AlterColumnType),
        Box::new(rename::Rename),
    ]
});

/// All rules enabled in this build, in stable registration order.
pub fn all_rules() -> &'static [Box<dyn Rule>] {
    &RULES
}
