//! Policy lint (configured, off until set): flag a column whose type is in the configured
//! `[forbidden-types]` set. Matching canonicalizes both the configured type and the column's type
//! through the PostgreSQL parser, so any spelling matches its canonical form. Engine-synthesized;
//! runs only when forbidden types are configured. Not a registered `Rule`.

use std::collections::BTreeMap;

use pg_query::protobuf::RawStmt;

use crate::rules::{column_base_type, defined_columns};

pub(crate) const ID: &str = "forbidden-column-type";
pub(crate) const GUIDANCE: &str =
    "Change the column to an allowed type, or remove this type from the [forbidden-types] section of \
     your config.";

/// The canonical base-type name PostgreSQL normalizes `spelling` to (e.g. `char` → `bpchar`,
/// `integer` → `int4`), or `None` if `spelling` cannot be parsed as a type name. The parser has no
/// catalog, so it does NOT validate that the type exists: a bare unknown identifier passes through
/// unchanged (`notatype` → `Some("notatype")`), exactly like a real type (`money`) or an extension
/// type (`citext`). Such a type is simply inert when matched against columns.
pub(crate) fn canonical_type(spelling: &str) -> Option<String> {
    let sql = format!("CREATE TABLE _pgsafe_typecheck (c {spelling})");
    let parsed = pg_query::parse(&sql).ok()?;
    let node = parsed
        .protobuf
        .stmts
        .first()?
        .stmt
        .as_ref()?
        .node
        .as_ref()?;
    let col = defined_columns(node).into_iter().next()?;
    column_base_type(col)
}

/// Introduced columns whose type is forbidden, as `(statement_index, message)`. `forbidden` maps a
/// configured type spelling to its suggested replacement (empty string = no suggestion).
pub(crate) fn forbidden_violations(
    stmts: &[RawStmt],
    forbidden: &BTreeMap<String, String>,
) -> Vec<(usize, String)> {
    // canonical leaf -> (configured spelling, replacement). Built once. A spelling that fails to
    // canonicalize is skipped (config validation already rejects those).
    let lookup: BTreeMap<String, (&str, &str)> = forbidden
        .iter()
        .filter_map(|(ty, repl)| canonical_type(ty).map(|c| (c, (ty.as_str(), repl.as_str()))))
        .collect();
    if lookup.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        for col in defined_columns(node) {
            let Some(base) = column_base_type(col) else {
                continue;
            };
            if let Some((spelling, replacement)) = lookup.get(&base) {
                let message = if replacement.is_empty() {
                    format!(
                        "The column `{}` uses `{spelling}`, which your policy disallows.",
                        col.colname
                    )
                } else {
                    format!(
                        "The column `{}` uses `{spelling}`, which your policy disallows; use \
                         `{replacement}` instead.",
                        col.colname
                    )
                };
                out.push((i, message));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{canonical_type, forbidden_violations};
    use crate::{lint_sql, LintOptions};

    fn forbid(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn flagged(sql: &str, pairs: &[(&str, &str)]) -> Vec<String> {
        forbidden_violations(
            &pg_query::parse(sql).unwrap().protobuf.stmts,
            &forbid(pairs),
        )
        .into_iter()
        .map(|(_, m)| m)
        .collect()
    }

    #[test]
    fn canonical_type_normalizes_spellings() {
        assert_eq!(canonical_type("char").as_deref(), Some("bpchar"));
        assert_eq!(canonical_type("integer").as_deref(), Some("int4"));
        assert_eq!(
            canonical_type("timestamp with time zone").as_deref(),
            Some("timestamptz")
        );
    }

    #[test]
    fn canonical_type_returns_none_on_unparseable() {
        // Only strings that fail to PARSE as a type yield None. A bare unknown identifier passes
        // through (`notatype` -> Some) — the parser has no catalog to validate type existence.
        assert!(canonical_type("not a real type").is_none());
        assert_eq!(canonical_type("notatype").as_deref(), Some("notatype"));
    }

    #[test]
    fn unrecognized_type_is_inert() {
        // A configured type the parser doesn't recognize matches no column — no finding, no error.
        assert!(flagged("CREATE TABLE t (c text)", &[("notatype", "")]).is_empty());
    }

    #[test]
    fn forbidden_type_is_flagged_with_replacement() {
        let f = flagged(
            "CREATE TABLE t (created timestamp)",
            &[("timestamp", "timestamptz")],
        );
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("`timestamptz`"));
    }

    #[test]
    fn distinct_canonical_type_is_not_flagged() {
        // forbidding `timestamp` must not flag a `timestamptz` column.
        assert!(flagged(
            "CREATE TABLE t (created timestamptz)",
            &[("timestamp", "timestamptz")]
        )
        .is_empty());
    }

    #[test]
    fn spelling_matches_via_canonicalization() {
        // config `char` matches a char(10) column (both canonicalize to bpchar).
        assert_eq!(
            flagged("CREATE TABLE t (code char(10))", &[("char", "text")]).len(),
            1
        );
    }

    #[test]
    fn add_column_forbidden_type_is_flagged() {
        assert_eq!(
            flagged("ALTER TABLE t ADD COLUMN c money", &[("money", "numeric")]).len(),
            1
        );
    }

    #[test]
    fn allowed_type_is_silent() {
        assert!(flagged(
            "CREATE TABLE t (id bigint)",
            &[("timestamp", "timestamptz")]
        )
        .is_empty());
    }

    #[test]
    fn empty_config_does_no_work() {
        assert!(flagged("CREATE TABLE t (created timestamp)", &[]).is_empty());
    }

    #[test]
    fn empty_replacement_omits_suggestion() {
        let f = flagged("CREATE TABLE t (c money)", &[("money", "")]);
        assert_eq!(f.len(), 1);
        assert!(!f[0].contains("use `"));
    }

    fn forbid_opts(pairs: &[(&str, &str)]) -> LintOptions {
        LintOptions {
            forbidden_column_types: forbid(pairs),
            ..LintOptions::default()
        }
    }

    #[test]
    fn silent_without_config() {
        let f = lint_sql(
            "CREATE TABLE t (created timestamp)",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(f.iter().all(|f| f.rule_id != "forbidden-column-type"));
    }

    #[test]
    fn fires_with_config() {
        use crate::Severity;
        let f = lint_sql(
            "CREATE TABLE t (created timestamp)",
            &forbid_opts(&[("timestamp", "timestamptz")]),
        )
        .unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "forbidden-column-type")
            .expect("rule must fire when configured");
        assert_eq!(hit.severity, Severity::Warning);
    }

    #[test]
    fn passes_on_allowed_type() {
        let f = lint_sql(
            "CREATE TABLE t (created timestamptz)",
            &forbid_opts(&[("timestamp", "timestamptz")]),
        )
        .unwrap();
        assert!(f.iter().all(|f| f.rule_id != "forbidden-column-type"));
    }

    #[test]
    fn inline_suppressible() {
        let sql = "-- pgsafe:ignore forbidden-column-type legacy column\n\
                   CREATE TABLE t (created timestamp)";
        let f = lint_sql(sql, &forbid_opts(&[("timestamp", "timestamptz")])).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "forbidden-column-type")
            .expect("rule must fire");
        assert!(hit.is_suppressed());
    }
}
