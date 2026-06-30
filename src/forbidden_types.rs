//! Policy lint (configured, off until set): flag a column whose type is in the configured
//! `[forbidden-types]` set. Matching canonicalizes both the configured type and the column's type
//! through the PostgreSQL parser, so any spelling matches its canonical form. Engine-synthesized;
//! runs only when forbidden types are configured. Not a registered `Rule`.

use std::collections::BTreeMap;

use pg_query::protobuf::{ColumnDef, RawStmt};

use crate::fix::{FixAnchor, FixDraft, FixDraftEdit};
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

/// Returns `true` when the type token starting at byte `at` in `sql` is a single SQL word —
/// not a multi-word type like `timestamp without time zone` or `double precision`, and not
/// schema-qualified like `pg_catalog.text`.
///
/// Accepted risk: a schema-qualified custom type (`myschema.mytype`) written without
/// qualification cannot be distinguished here from a bare single-word type; the `.` guard
/// conservatively suppresses the fix only when a `.` follows the first word, which is the
/// right safe-by-default behaviour for `ReplaceTokenAt`.
fn is_single_token_type(sql: &str, at: usize) -> bool {
    let Some(rest) = sql.get(at..) else {
        return false;
    };
    let tok_len = rest
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    if tok_len == 0 {
        return false;
    }
    let after = rest[tok_len..].trim_start();
    // A continuation word or schema-qualifier dot means the written type spans multiple tokens.
    const CONT: [&str; 6] = ["with", "without", "varying", "precision", "time", "zone"];
    if after.starts_with('.') {
        return false;
    }
    let next_word: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    !CONT.iter().any(|w| w.eq_ignore_ascii_case(&next_word))
}

/// Build a fix draft that swaps the column's type token to `replacement`.
///
/// Guards:
/// - `replacement` must be non-empty (empty means "no suggestion" in the config).
/// - `type_name` must be present and `location >= 0` (pg_query sets -1 for unknown positions).
/// - The written type must be a single SQL token; multi-word types like
///   `timestamp without time zone` or `double precision` are rejected because
///   `ReplaceTokenAt` replaces only the first word, which would corrupt the rest.
///
/// `location` points at the source token regardless of how many names pg_query
/// normalises the type into (e.g. `timestamp` → `["pg_catalog", "timestamp"]`), so
/// the names-list length is not checked.
fn forbidden_fix(sql: &str, replacement: &str, col: &ColumnDef) -> Option<FixDraft> {
    if replacement.is_empty() {
        return None;
    }
    let tn = col.type_name.as_ref()?;
    // Reject location == -1 (unknown source position) via the sign-checked conversion.
    // Using try_from avoids the cast_sign_loss clippy lint that `as u32` would trigger.
    let at = u32::try_from(tn.location).ok()?;
    if !is_single_token_type(sql, at as usize) {
        return None;
    }
    Some(FixDraft {
        title: "Change to configured type",
        edits: vec![FixDraftEdit {
            anchor: FixAnchor::ReplaceTokenAt(at),
            replacement: replacement.into(),
        }],
    })
}

/// Introduced columns whose type is forbidden, as `(statement_index, message, fix_draft)`.
/// `forbidden` maps a configured type spelling to its suggested replacement (empty = no suggestion).
pub(crate) fn forbidden_violations(
    stmts: &[RawStmt],
    forbidden: &BTreeMap<String, String>,
    sql: &str,
) -> Vec<(usize, String, Option<FixDraft>)> {
    // canonical leaf -> (configured spelling, replacement). Built once. A spelling that fails to
    // canonicalize is skipped silently — an unrecognized type matches nothing.
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
                let fix = forbidden_fix(sql, replacement, col);
                out.push((i, message, fix));
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
            sql,
        )
        .into_iter()
        .map(|(_, m, _)| m)
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
    fn severity_override_escalates_to_error() {
        use crate::Severity;
        let opts = LintOptions {
            forbidden_column_types: forbid(&[("timestamp", "timestamptz")]),
            severity_overrides: [("forbidden-column-type".to_string(), Severity::Error)]
                .into_iter()
                .collect(),
            ..LintOptions::default()
        };
        let f = lint_sql("CREATE TABLE t (created timestamp)", &opts).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "forbidden-column-type")
            .expect("rule must fire");
        assert_eq!(hit.severity, Severity::Error);
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

    // --- fix producer tests (TDD: written before implementation) ---

    #[test]
    fn timestamp_fix_clears_rule() {
        use crate::fix::apply;
        let sql = "CREATE TABLE t (created timestamp)";
        let fs = lint_sql(sql, &forbid_opts(&[("timestamp", "timestamptz")])).unwrap();
        let hit = fs
            .iter()
            .find(|f| f.rule_id == "forbidden-column-type")
            .expect("rule must fire");
        let fix = hit
            .fix
            .as_ref()
            .expect("fix must be present for single-token type");
        let fixed = apply(sql, fix);
        assert_eq!(fixed, "CREATE TABLE t (created timestamptz)");
        assert!(
            lint_sql(&fixed, &forbid_opts(&[("timestamp", "timestamptz")]))
                .unwrap()
                .iter()
                .all(|f| f.rule_id != "forbidden-column-type"),
            "fixed SQL must not re-trigger forbidden-column-type"
        );
    }

    #[test]
    fn add_column_money_fix_clears_rule() {
        use crate::fix::apply;
        let sql = "ALTER TABLE t ADD COLUMN c money";
        let fs = lint_sql(sql, &forbid_opts(&[("money", "numeric")])).unwrap();
        let hit = fs
            .iter()
            .find(|f| f.rule_id == "forbidden-column-type")
            .expect("rule must fire");
        let fix = hit
            .fix
            .as_ref()
            .expect("fix must be present for single-token type");
        let fixed = apply(sql, fix);
        assert_eq!(fixed, "ALTER TABLE t ADD COLUMN c numeric");
        assert!(
            lint_sql(&fixed, &forbid_opts(&[("money", "numeric")]))
                .unwrap()
                .iter()
                .all(|f| f.rule_id != "forbidden-column-type"),
            "fixed SQL must not re-trigger forbidden-column-type"
        );
    }

    #[test]
    fn multi_word_type_fix_is_none() {
        // `timestamp without time zone` is a multi-word type; ReplaceTokenAt would replace
        // only the first word, leaving `timestamptz without time zone` — a corrupt rewrite.
        // The single-token guard must suppress the fix draft while still firing the finding.
        let sql = "CREATE TABLE t (created timestamp without time zone)";
        let fs = lint_sql(sql, &forbid_opts(&[("timestamp", "timestamptz")])).unwrap();
        let hit = fs
            .iter()
            .find(|f| f.rule_id == "forbidden-column-type")
            .expect("rule must fire — the finding is still valid");
        assert!(
            hit.fix.is_none(),
            "fix must be suppressed for multi-word type (single-token guard)"
        );
    }
}
