use pg_query::protobuf::ConstrType;
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddUniqueConstraint;

impl Rule for AddUniqueConstraint {
    fn id(&self) -> &'static str {
        "add-unique-constraint"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        let message =
            "Adding a UNIQUE constraint inline builds its underlying index while holding \
                       ACCESS EXCLUSIVE on the table for the whole build.";

        // `ADD CONSTRAINT ... UNIQUE` that builds a fresh index (no `USING INDEX`).
        let via_constraint = super::constraints_being_added(node).into_iter().any(|c| {
            matches!(
                ConstrType::try_from(c.contype),
                Ok(ConstrType::ConstrUnique)
            ) && c.indexname.is_empty()
        });
        if via_constraint {
            out.push(RuleHit {
                message: message.into(),
                guidance: "Build the index first with CREATE UNIQUE INDEX CONCURRENTLY, then attach it: \
                           ALTER TABLE ... ADD CONSTRAINT ... UNIQUE USING INDEX idx (a brief lock)."
                    .into(),
            });
        }

        // `ADD COLUMN ... UNIQUE` — the column does not exist yet, so the index must be created after
        // it, not before; the safe sequence differs from the `ADD CONSTRAINT` form above.
        let via_column = super::columns_being_added(node)
            .into_iter()
            .any(|col| super::column_has_constraint(col, ConstrType::ConstrUnique));
        if via_column {
            out.push(RuleHit {
                message: message.into(),
                guidance: "Add the column without UNIQUE first, then CREATE UNIQUE INDEX CONCURRENTLY on \
                           it, then ALTER TABLE ... ADD CONSTRAINT ... UNIQUE USING INDEX idx (a brief \
                           lock)."
                    .into(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions};

    fn hits(sql: &str) -> Vec<String> {
        lint_sql(sql, &LintOptions::default())
            .unwrap()
            .into_iter()
            .filter(|f| f.rule_id == "add-unique-constraint")
            .map(|f| f.guidance)
            .collect()
    }

    #[test]
    fn flags_add_constraint_unique() {
        let g = hits("ALTER TABLE t ADD CONSTRAINT uq UNIQUE (email)");
        assert_eq!(g.len(), 1);
        assert!(g[0].contains("USING INDEX"));
    }

    #[test]
    fn flags_add_column_unique_with_column_specific_guidance() {
        let g = hits("ALTER TABLE t ADD COLUMN email text UNIQUE");
        assert_eq!(g.len(), 1);
        assert!(g[0].contains("Add the column without UNIQUE first"));
    }

    #[test]
    fn ignores_add_constraint_using_index() {
        // promoting an already-built index is the safe path and must not fire.
        assert!(hits("ALTER TABLE t ADD CONSTRAINT uq UNIQUE USING INDEX uq_idx").is_empty());
    }

    #[test]
    fn ignores_non_unique_constraint() {
        assert!(hits("ALTER TABLE t ADD CONSTRAINT ck CHECK (x > 0)").is_empty());
    }
}
