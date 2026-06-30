use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct RefreshMatviewNonConcurrent;

impl Rule for RefreshMatviewNonConcurrent {
    fn id(&self) -> &'static str {
        "refresh-matview-non-concurrent"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        if let NodeEnum::RefreshMatViewStmt(r) = node {
            // WITH NO DATA (skip_data) just empties the matview and is fast; CONCURRENTLY is the
            // safe rebuild form, so neither is the hazard.
            if !r.concurrent && !r.skip_data {
                out.push(RuleHit {
                    message:
                        "REFRESH MATERIALIZED VIEW without CONCURRENTLY takes an ACCESS EXCLUSIVE \
                              lock and blocks all reads of the view while it rebuilds."
                            .into(),
                    guidance:
                        "Use REFRESH MATERIALIZED VIEW CONCURRENTLY (requires a unique index on \
                               the matview) so reads are not blocked during the rebuild."
                            .into(),
                    fix: None,
                });
            }
        }
    }
}
