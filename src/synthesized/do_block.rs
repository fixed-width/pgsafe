//! Policy lint (opt-in, off by default): flag a `DO` block that contains SQL pgsafe cannot statically
//! analyze — a dynamic `EXECUTE`, or a body that would not parse. Static statements inside a `DO`
//! block are linted by default (see [`super::plpgsql`]); this rule surfaces only the residual blind
//! spot. The residue signal is computed in `lint_sql`; this module holds the rule's identity and copy.

/// The rule id.
pub(crate) const ID: &str = "unchecked-do-block";
/// The finding message for an un-analyzable `DO` block.
pub(crate) const MESSAGE: &str =
    "This DO block contains dynamic SQL (EXECUTE) or a body pgsafe could not parse; the rest of the \
     block was checked, but that part was not.";
/// Safe-rewrite guidance.
pub(crate) const GUIDANCE: &str =
    "Move the dynamic DDL/DML out of the DO block into top-level statements so pgsafe can check it, or \
     suppress this finding with an inline `-- pgsafe:ignore unchecked-do-block <reason>` after review.";

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions};

    fn enabled() -> LintOptions {
        LintOptions {
            enabled_rules: ["unchecked-do-block".to_string()].into_iter().collect(),
            ..LintOptions::default()
        }
    }

    fn has_unchecked(sql: &str, opts: &LintOptions) -> bool {
        lint_sql(sql, opts)
            .unwrap()
            .iter()
            .any(|f| f.rule_id == "unchecked-do-block")
    }

    #[test]
    fn fully_analyzable_block_has_no_residue_finding_even_when_enabled() {
        // only direct execsql — nothing un-analyzable, so the residue rule must stay silent.
        assert!(!has_unchecked(
            "DO $$ BEGIN ALTER TABLE t ADD COLUMN x int; END $$;",
            &enabled()
        ));
    }

    #[test]
    fn dynamic_execute_fires_residue_when_enabled() {
        assert!(has_unchecked(
            "DO $$ BEGIN EXECUTE 'ALTER TABLE t ADD COLUMN x int'; END $$;",
            &enabled()
        ));
    }

    #[test]
    fn unparsable_body_fires_residue_when_enabled() {
        // missing END IF — parse_plpgsql rejects the body.
        assert!(has_unchecked(
            "DO $$ BEGIN IF true THEN NULL; END $$;",
            &enabled()
        ));
    }

    #[test]
    fn off_by_default() {
        assert!(!has_unchecked(
            "DO $$ BEGIN EXECUTE 'ALTER TABLE t ADD COLUMN x int'; END $$;",
            &LintOptions::default(),
        ));
    }

    #[test]
    fn fires_with_warning_severity() {
        use crate::Severity;
        let f = lint_sql(
            "DO $$ BEGIN EXECUTE 'ALTER TABLE t ADD COLUMN x int'; END $$;",
            &enabled(),
        )
        .unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "unchecked-do-block")
            .expect("rule must fire on residue when enabled");
        assert_eq!(hit.severity, Severity::Warning);
    }

    #[test]
    fn suppressible_when_enabled() {
        let sql = "-- pgsafe:ignore unchecked-do-block reviewed\n\
                   DO $$ BEGIN EXECUTE 'ALTER TABLE t ADD COLUMN x int'; END $$;";
        let f = lint_sql(sql, &enabled()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "unchecked-do-block")
            .expect("rule must fire");
        assert!(hit.is_suppressed());
    }

    #[test]
    fn disabled_overrides_enabled() {
        let opts = LintOptions {
            enabled_rules: ["unchecked-do-block".to_string()].into_iter().collect(),
            disabled_rules: ["unchecked-do-block".to_string()].into_iter().collect(),
            ..LintOptions::default()
        };
        let f = lint_sql(
            "DO $$ BEGIN EXECUTE 'ALTER TABLE t ADD COLUMN x int'; END $$;",
            &opts,
        )
        .unwrap();
        assert!(f.iter().all(|f| f.rule_id != "unchecked-do-block"));
    }
}
