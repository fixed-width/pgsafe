use pg_query::protobuf::ColumnDef;
use pg_query::NodeEnum;

use super::Rule;
use crate::fix::{FixAnchor, FixDraft, FixDraftEdit};
use crate::RuleHit;

pub struct PreferJsonb;

/// Build a fix draft that swaps the `json` type token to `jsonb`.
///
/// Guards: `type_name` must be `Some` and `location >= 0` (pg_query sets -1 when the
/// source position is unknown).
fn jsonb_fix(col: &ColumnDef) -> Option<FixDraft> {
    let tn = col.type_name.as_ref()?;
    // pg_query normalises even unqualified built-in types to a two-element names list
    // (e.g. `json` → `["pg_catalog", "json"]`), but `location` always points at the
    // token as written in the source text — so for unqualified `json`, `location`
    // correctly lands on the `j` in `json`.  We rely on that here.
    //
    // pg_query sets location to -1 when the source position is unknown; those are
    // rejected by the `try_from` conversion returning `None`.
    let at = u32::try_from(tn.location).ok()?;
    // NOTE: a user who explicitly writes the catalog-qualified form (e.g. `pg_catalog.json`)
    // would have `location` point at `pg_catalog`, so the replacement would corrupt the output.
    // We accept this: catalog-qualifying a built-in type is essentially unheard of in real DDL.
    Some(FixDraft {
        title: "Use jsonb",
        edits: vec![FixDraftEdit {
            anchor: FixAnchor::ReplaceTokenAt(at),
            replacement: "jsonb".into(),
        }],
    })
}

impl Rule for PreferJsonb {
    fn id(&self) -> &'static str {
        "prefer-jsonb"
    }
    // severity() defaults to Warning.
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for col in super::defined_columns(node) {
            if super::column_base_type(col).as_deref() == Some("json") {
                out.push(RuleHit {
                    message: "A `json` column has no equality or ordering operators, so \
                              SELECT DISTINCT, GROUP BY, UNION, and ORDER BY on it fail at query time."
                        .into(),
                    guidance: "Use `jsonb` instead — it supports those operators and indexing. `json` \
                               only preserves exact input text and duplicate/key order, which is rarely \
                               needed."
                        .into(),
                    fix: jsonb_fix(col),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions};

    fn fires(sql: &str) -> bool {
        lint_sql(sql, &LintOptions::default())
            .unwrap()
            .iter()
            .any(|f| f.rule_id == "prefer-jsonb")
    }

    #[test]
    fn flags_json_in_create_table() {
        assert!(fires("CREATE TABLE t (id int, data json)"));
    }
    #[test]
    fn flags_json_in_add_column() {
        assert!(fires("ALTER TABLE t ADD COLUMN data json"));
    }
    #[test]
    fn ignores_jsonb() {
        assert!(!fires("CREATE TABLE t (id int, data jsonb)"));
        assert!(!fires("ALTER TABLE t ADD COLUMN data jsonb"));
    }

    #[test]
    fn emits_jsonb_fix_on_add_column_and_clears() {
        use crate::fix::apply;
        let sql = "ALTER TABLE t ADD COLUMN data json;";
        let fs = lint_sql(sql, &LintOptions::default()).unwrap();
        let f = fs.iter().find(|f| f.rule_id == "prefer-jsonb").unwrap();
        let fix = f.fix.as_ref().expect("fix present");
        assert_eq!(fix.title, "Use jsonb");
        let fixed = apply(sql, fix);
        assert_eq!(fixed, "ALTER TABLE t ADD COLUMN data jsonb;");
        assert!(
            lint_sql(&fixed, &LintOptions::default())
                .unwrap()
                .iter()
                .all(|f| f.rule_id != "prefer-jsonb"),
            "fixed SQL must not re-trigger prefer-jsonb"
        );
    }

    #[test]
    fn emits_jsonb_fix_on_create_table_and_clears() {
        use crate::fix::apply;
        let sql = "CREATE TABLE t (id int, data json)";
        let fs = lint_sql(sql, &LintOptions::default()).unwrap();
        let f = fs.iter().find(|f| f.rule_id == "prefer-jsonb").unwrap();
        let fix = f.fix.as_ref().expect("fix present");
        let fixed = apply(sql, fix);
        assert_eq!(fixed, "CREATE TABLE t (id int, data jsonb)");
        assert!(
            lint_sql(&fixed, &LintOptions::default())
                .unwrap()
                .iter()
                .all(|f| f.rule_id != "prefer-jsonb"),
            "fixed SQL must not re-trigger prefer-jsonb"
        );
    }
}
