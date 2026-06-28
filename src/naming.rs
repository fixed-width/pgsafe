//! Parameterized policy lint: flag an introduced identifier whose name does not match the configured
//! regex for its kind (the `[naming]` config). Engine-synthesized; runs only when naming patterns are
//! configured. Not a registered `Rule`.

use std::collections::BTreeMap;

use pg_query::protobuf::{ObjectType, RawStmt};
use pg_query::NodeEnum;

use crate::NameKind;

pub(crate) const ID: &str = "naming-convention";
pub(crate) const GUIDANCE: &str =
    "Rename the object to match the convention, or adjust the [naming] pattern in your config.";

/// The `NameKind` a `RENAME` targets, from its `rename_type`. `None` for kinds we don't check.
fn rename_kind(rename_type: i32) -> Option<NameKind> {
    match ObjectType::try_from(rename_type) {
        Ok(ObjectType::ObjectTable) => Some(NameKind::Table),
        Ok(ObjectType::ObjectColumn) => Some(NameKind::Column),
        Ok(ObjectType::ObjectIndex) => Some(NameKind::Index),
        Ok(ObjectType::ObjectTabconstraint) => Some(NameKind::Constraint),
        Ok(ObjectType::ObjectSequence) => Some(NameKind::Sequence),
        Ok(ObjectType::ObjectTrigger) => Some(NameKind::Trigger),
        Ok(ObjectType::ObjectSchema) => Some(NameKind::Schema),
        _ => None,
    }
}

/// The names a statement introduces, tagged by kind. Empty names are skipped.
fn introduced_names(node: &NodeEnum) -> Vec<(NameKind, String)> {
    let mut out: Vec<(NameKind, String)> = Vec::new();
    match node {
        NodeEnum::CreateStmt(c) => {
            if let Some(rv) = c.relation.as_ref() {
                out.push((NameKind::Table, rv.relname.clone()));
            }
            for col in crate::rules::defined_columns(node) {
                out.push((NameKind::Column, col.colname.clone()));
                for con in &col.constraints {
                    if let Some(NodeEnum::Constraint(cn)) = con.node.as_ref() {
                        out.push((NameKind::Constraint, cn.conname.clone()));
                    }
                }
            }
            for con in crate::rules::defined_table_constraints(node) {
                out.push((NameKind::Constraint, con.conname.clone()));
            }
        }
        NodeEnum::AlterTableStmt(_) => {
            for col in crate::rules::defined_columns(node) {
                out.push((NameKind::Column, col.colname.clone()));
            }
            for con in crate::rules::defined_table_constraints(node) {
                out.push((NameKind::Constraint, con.conname.clone()));
            }
        }
        NodeEnum::IndexStmt(i) => out.push((NameKind::Index, i.idxname.clone())),
        NodeEnum::CreateTrigStmt(t) => out.push((NameKind::Trigger, t.trigname.clone())),
        NodeEnum::CreateSeqStmt(s) => {
            if let Some(rv) = s.sequence.as_ref() {
                out.push((NameKind::Sequence, rv.relname.clone()));
            }
        }
        NodeEnum::CreateSchemaStmt(s) => out.push((NameKind::Schema, s.schemaname.clone())),
        NodeEnum::RenameStmt(r) => {
            if let Some(kind) = rename_kind(r.rename_type) {
                out.push((kind, r.newname.clone()));
            }
        }
        _ => {}
    }
    out.retain(|(_, name)| !name.is_empty());
    out
}

/// Introduced names that violate their kind's configured pattern, as `(statement_index, message)`.
pub(crate) fn naming_violations(
    stmts: &[RawStmt],
    patterns: &BTreeMap<NameKind, String>,
) -> Vec<(usize, String)> {
    // Compile the configured patterns once. A pattern that fails to compile is skipped (config
    // validation already rejects bad patterns at load).
    let compiled: BTreeMap<NameKind, regex::Regex> = patterns
        .iter()
        .filter_map(|(k, p)| regex::Regex::new(p).ok().map(|re| (*k, re)))
        .collect();
    if compiled.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        for (kind, name) in introduced_names(node) {
            if let Some(re) = compiled.get(&kind) {
                if !re.is_match(&name) {
                    out.push((
                        i,
                        format!(
                            "The {} name `{name}` does not match the configured naming pattern `{}`.",
                            kind.as_str(),
                            patterns[&kind]
                        ),
                    ));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::naming_violations;
    use crate::NameKind;

    fn violations(sql: &str, patterns: &[(NameKind, &str)]) -> Vec<String> {
        let p: BTreeMap<NameKind, String> = patterns
            .iter()
            .map(|(k, v)| (*k, (*v).to_string()))
            .collect();
        naming_violations(&pg_query::parse(sql).unwrap().protobuf.stmts, &p)
            .into_iter()
            .map(|(_, m)| m)
            .collect()
    }

    #[test]
    fn table_name_mismatch_is_flagged() {
        assert_eq!(
            violations("CREATE TABLE users (id int)", &[(NameKind::Table, "^t_")]).len(),
            1
        );
        assert!(
            violations("CREATE TABLE t_users (id int)", &[(NameKind::Table, "^t_")]).is_empty()
        );
    }

    #[test]
    fn column_name_mismatch_is_flagged() {
        // quoted "Id" keeps its case; snake_case pattern rejects it.
        assert_eq!(
            violations(
                "CREATE TABLE t (\"Id\" int)",
                &[(NameKind::Column, "^[a-z][a-z0-9_]*$")]
            )
            .len(),
            1
        );
    }

    #[test]
    fn index_and_constraint_and_sequence_and_trigger_and_schema() {
        assert_eq!(
            violations("CREATE INDEX foo ON t (x)", &[(NameKind::Index, "^ix_")]).len(),
            1
        );
        assert_eq!(
            violations(
                "ALTER TABLE t ADD CONSTRAINT bad CHECK (x > 0)",
                &[(NameKind::Constraint, "^ck_")]
            )
            .len(),
            1
        );
        assert_eq!(
            violations("CREATE SEQUENCE foo", &[(NameKind::Sequence, "^seq_")]).len(),
            1
        );
        assert_eq!(
            violations(
                "CREATE TRIGGER foo AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()",
                &[(NameKind::Trigger, "^trg_")]
            )
            .len(),
            1
        );
        assert_eq!(
            violations("CREATE SCHEMA bad", &[(NameKind::Schema, "^app_")]).len(),
            1
        );
    }

    #[test]
    fn rename_target_is_checked() {
        assert_eq!(
            violations(
                "ALTER TABLE t RENAME TO \"Bad\"",
                &[(NameKind::Table, "^[a-z]")]
            )
            .len(),
            1
        );
    }

    #[test]
    fn kind_without_a_pattern_is_not_checked() {
        // only a column pattern is set; the bad table name is not checked.
        assert!(violations(
            "CREATE TABLE \"Bad\" (id int)",
            &[(NameKind::Column, "^[a-z]")]
        )
        .is_empty());
    }

    #[test]
    fn no_patterns_means_no_work() {
        assert!(violations("CREATE TABLE \"Bad\" (id int)", &[]).is_empty());
    }

    use crate::{lint_sql, LintOptions};

    fn table_pat_opts() -> LintOptions {
        LintOptions {
            naming_patterns: [(NameKind::Table, "^t_".to_string())].into_iter().collect(),
            ..LintOptions::default()
        }
    }

    #[test]
    fn silent_without_patterns() {
        let f = lint_sql("CREATE TABLE users (id int)", &LintOptions::default()).unwrap();
        assert!(f.iter().all(|f| f.rule_id != "naming-convention"));
    }

    #[test]
    fn fires_with_a_pattern() {
        use crate::Severity;
        let f = lint_sql("CREATE TABLE users (id int)", &table_pat_opts()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "naming-convention")
            .expect("rule must fire on a mismatch");
        assert_eq!(hit.severity, Severity::Warning);
    }

    #[test]
    fn passes_on_a_match() {
        let f = lint_sql("CREATE TABLE t_users (id int)", &table_pat_opts()).unwrap();
        assert!(f.iter().all(|f| f.rule_id != "naming-convention"));
    }

    #[test]
    fn inline_suppressible() {
        let sql = "-- pgsafe:ignore naming-convention legacy table\nCREATE TABLE users (id int)";
        let f = lint_sql(sql, &table_pat_opts()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "naming-convention")
            .expect("rule must fire");
        assert!(hit.is_suppressed());
    }
}
