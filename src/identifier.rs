//! Identifier-length check: flag any identifier written longer than PostgreSQL's
//! 63-byte limit (`NAMEDATALEN - 1`). PostgreSQL silently truncates over-long
//! identifiers, so two names sharing a 63-byte prefix collide. This MUST use
//! `pg_query::scan()` rather than the parsed AST: libpg_query runs PostgreSQL's real
//! scanner, which truncates identifiers to 63 bytes at parse time, so an over-long
//! name is already shortened in the AST and an AST length check can never fire. The
//! scanner's token spans point into the raw source, preserving the true length. This
//! is an engine-synthesized finding, not a registered `Rule`.

use pg_query::protobuf::Token;

pub(crate) const ID: &str = "identifier-too-long";

/// `NAMEDATALEN - 1`: PostgreSQL stores identifiers in a fixed 64-byte field and
/// silently truncates anything longer to 63 bytes.
const MAX_IDENTIFIER_BYTES: usize = 63;

/// Every identifier token in `sql` whose true byte length exceeds 63, as
/// `(byte_offset, written_form, byte_len)`. Measured from the raw source via
/// `pg_query::scan()`, so the length is the pre-truncation one. Returns an empty
/// vec if the input cannot be scanned (it has already parsed successfully by the
/// time this runs, so that is not expected).
pub(crate) fn long_identifiers(sql: &str) -> Vec<(usize, String, usize)> {
    let Ok(scan) = pg_query::scan(sql) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for t in &scan.tokens {
        if t.token != Token::Ident as i32 {
            continue;
        }
        let (start, end) = (
            usize::try_from(t.start).unwrap_or(0),
            usize::try_from(t.end).unwrap_or(0),
        );
        let Some(slice) = sql.get(start..end) else {
            continue;
        };
        let len = identifier_byte_len(slice);
        if len > MAX_IDENTIFIER_BYTES {
            out.push((start, slice.to_string(), len));
        }
    }
    out
}

/// The true byte length an identifier token represents. For a quoted identifier
/// (`"..."`) the surrounding quotes are removed and each doubled `""` (an escaped
/// quote) counts as a single byte; otherwise it is the slice's byte length.
fn identifier_byte_len(slice: &str) -> usize {
    if slice.len() >= 2 && slice.starts_with('"') && slice.ends_with('"') {
        let inner = &slice[1..slice.len() - 1];
        inner.len() - inner.matches("\"\"").count()
    } else {
        slice.len()
    }
}

#[cfg(test)]
mod tests {
    use super::long_identifiers;

    fn fires(sql: &str) -> bool {
        !long_identifiers(sql).is_empty()
    }
    fn long() -> String {
        "a".repeat(64)
    }
    fn at_limit() -> String {
        "a".repeat(63)
    }

    #[test]
    fn flags_over_long_table_name() {
        assert!(fires(&format!("CREATE TABLE {} (id int)", long())));
    }

    #[test]
    fn flags_over_long_column_name() {
        assert!(fires(&format!("CREATE TABLE t ({} int)", long())));
    }

    #[test]
    fn flags_over_long_constraint_name() {
        assert!(fires(&format!(
            "ALTER TABLE t ADD CONSTRAINT {} CHECK (x > 0)",
            long()
        )));
    }

    #[test]
    fn flags_over_long_rename_target() {
        assert!(fires(&format!("ALTER TABLE t RENAME TO {}", long())));
    }

    #[test]
    fn flags_over_long_index_name() {
        assert!(fires(&format!("CREATE INDEX {} ON t (x)", long())));
    }

    #[test]
    fn flags_over_long_trigger_name() {
        assert!(fires(&format!(
            "CREATE TRIGGER {} AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()",
            long()
        )));
    }

    #[test]
    fn silent_at_63_byte_limit() {
        assert!(!fires(&format!("CREATE TABLE {} (id int)", at_limit())));
    }

    #[test]
    fn reports_the_true_untruncated_byte_length() {
        // The AST would show 63 (PostgreSQL truncates); scan() must report the real 64.
        let found = long_identifiers(&format!("CREATE TABLE {} (id int)", long()));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].2, 64, "byte_len must be the pre-truncation length");
    }

    #[test]
    fn byte_length_counts_multibyte_chars_not_chars() {
        // 32 × 'é' (U+00E9) = 32 chars but 64 UTF-8 bytes → over the 63-BYTE limit.
        let name = "é".repeat(32);
        assert_eq!(name.chars().count(), 32);
        assert_eq!(name.len(), 64);
        let found = long_identifiers(&format!("CREATE TABLE \"{name}\" (id int)"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].2, 64, "quotes must be stripped, content bytes counted");
    }

    #[test]
    fn quoted_identifier_quotes_are_not_counted() {
        // 63 inner bytes + 2 quotes = 65 source bytes; stripping the quotes leaves 63 → silent.
        let name = "a".repeat(63);
        assert!(!fires(&format!("CREATE TABLE \"{name}\" (id int)")));
    }

    #[test]
    fn lint_sql_emits_a_warning_for_an_over_long_identifier() {
        use crate::{lint_sql, LintOptions, Severity};
        let f = lint_sql(&format!("CREATE TABLE {} (id int)", long()), &LintOptions::default())
            .unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "identifier-too-long")
            .expect("rule must fire through the engine");
        assert_eq!(hit.severity, Severity::Warning);
    }

    #[test]
    fn identifier_finding_is_inline_suppressible() {
        use crate::{lint_sql, LintOptions};
        // CREATE TABLE avoids require-timeout, so the identifier finding is the only one.
        let sql = format!(
            "-- pgsafe:ignore identifier-too-long legacy name kept deliberately\n\
             CREATE TABLE {} (id int)",
            long()
        );
        let f = lint_sql(&sql, &LintOptions::default()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "identifier-too-long")
            .expect("rule must fire");
        assert!(hit.is_suppressed(), "directive must suppress the finding");
    }

    #[test]
    fn disabled_identifier_rule_is_silent() {
        use crate::{lint_sql, LintOptions};
        let opts = LintOptions {
            disabled_rules: ["identifier-too-long".to_string()].into_iter().collect(),
            ..LintOptions::default()
        };
        let f = lint_sql(&format!("CREATE TABLE {} (id int)", long()), &opts).unwrap();
        assert!(f.iter().all(|f| f.rule_id != "identifier-too-long"));
    }
}
