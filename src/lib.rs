#![deny(missing_docs)]
#![warn(
    clippy::missing_errors_doc,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]
//! `pgsafe` is a static safety linter for PostgreSQL DDL migrations.
//!
//! It parses SQL using the real PostgreSQL parser and checks every statement
//! against a set of rules that flag lock-taking or destructive operations,
//! producing a [`Finding`] for each match together with safe-rewrite guidance.
//!
//! The main entry point is [`lint_sql`]. A command-line interface wrapping
//! the same rules is provided by the `pgsafe` binary.

mod rules;

/// Severity level of a [`Finding`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The statement is potentially unsafe but may be acceptable in some contexts.
    Warning,
    /// The statement is unsafe and should not be run against a live database.
    Error,
}

/// Source position of a finding's statement: byte offset plus 1-based line and
/// (character) column of the statement's first non-whitespace token.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Location {
    /// 0-based byte offset of the statement's first non-whitespace character
    /// within the original SQL string.  Saturates to [`u32::MAX`] for inputs
    /// larger than 4 GiB.
    pub byte: u32,
    /// 1-based line number of the statement's first non-whitespace character.
    pub line: u32,
    /// 1-based character column of the statement's first non-whitespace
    /// character on its line.
    pub column: u32,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Warning => f.write_str("warning"),
            Severity::Error => f.write_str("error"),
        }
    }
}

/// What a rule emits when it matches. The engine adds identity and positional context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuleHit {
    pub message: String,
    pub guidance: String,
}

/// A safety finding reported for a specific SQL statement.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Finding {
    /// Identifier of the rule that triggered this finding (e.g. `"non-concurrent-index"`).
    pub rule_id: String,
    /// Severity level of the finding.
    pub severity: Severity,
    /// Short human-readable description of what is unsafe about the statement.
    pub message: String,
    /// Guidance on how to rewrite the statement safely.
    pub guidance: String,
    /// 0-based index of the SQL statement within the input that triggered this
    /// finding.  The human-readable CLI output renders this as `statement #0`,
    /// `statement #1`, etc.
    pub statement_index: usize,
    /// Source location of the statement's first non-whitespace token.
    pub location: Location,
    /// Trimmed text of the statement that triggered the finding.
    pub snippet: String,
}

/// Error returned when `pgsafe` cannot process the provided SQL.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LintError {
    /// The SQL input could not be parsed by the PostgreSQL parser.
    #[error("parse error: {0}")]
    Parse(#[source] pg_query::Error),
}

struct StatementSpan {
    location: Location,
    snippet: String,
}

/// Compute the trimmed snippet AND the corrected location (pointing at the
/// first non-whitespace byte of the statement, not the leading whitespace).
fn statement_span(sql: &str, raw: &pg_query::protobuf::RawStmt) -> StatementSpan {
    let off = usize::try_from(raw.stmt_location.max(0)).unwrap_or(0);
    let len = usize::try_from(raw.stmt_len.max(0)).unwrap_or(0);
    let end = if len == 0 {
        sql.len()
    } else {
        off.saturating_add(len).min(sql.len())
    };
    let raw_slice = sql.get(off..end).unwrap_or("");
    let lead_ws = raw_slice.len() - raw_slice.trim_start().len();
    let start = off + lead_ws;
    let snippet = raw_slice.trim().to_string();
    let byte = u32::try_from(start).unwrap_or(u32::MAX);
    let (line, column) = line_col(sql, start);
    StatementSpan {
        location: Location { byte, line, column },
        snippet,
    }
}

/// 1-based line and character-column of a byte offset within `sql`.
fn line_col(sql: &str, byte: usize) -> (u32, u32) {
    let mut line = 1u32;
    let mut column = 1u32;
    for (i, ch) in sql.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

/// Lints one or more SQL statements and returns every safety finding.
///
/// The SQL is parsed with the real PostgreSQL parser, so `sql` must be valid
/// PostgreSQL syntax.  Every statement in the input is checked against all
/// enabled rules; the returned [`Vec`] preserves source order.
///
/// # Errors
/// Returns [`LintError::Parse`] if `sql` cannot be parsed by the PostgreSQL parser.
///
/// # Examples
/// ```
/// let findings = pgsafe::lint_sql("CREATE INDEX i ON t (x)").unwrap();
/// assert!(!findings.is_empty());
/// ```
pub fn lint_sql(sql: &str) -> Result<Vec<Finding>, LintError> {
    let parsed = pg_query::parse(sql).map_err(LintError::Parse)?;
    let rules = rules::all_rules();
    let mut findings = Vec::new();
    let mut hits = Vec::new();

    for (i, raw) in parsed.protobuf.stmts.iter().enumerate() {
        let Some(stmt_box) = raw.stmt.as_ref() else {
            continue;
        };
        let Some(node) = stmt_box.node.as_ref() else {
            continue;
        };

        let span = statement_span(sql, raw);
        for rule in rules {
            hits.clear();
            rule.check(node, &mut hits);
            for h in hits.drain(..) {
                findings.push(Finding {
                    rule_id: rule.id().to_string(),
                    severity: rule.severity(),
                    message: h.message,
                    guidance: h.guidance,
                    statement_index: i,
                    location: span.location,
                    snippet: span.snippet.clone(),
                });
            }
        }
    }

    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_returns_no_findings() {
        assert_eq!(lint_sql("").unwrap(), Vec::new());
    }

    #[test]
    fn comment_only_returns_no_findings() {
        assert_eq!(lint_sql("-- just a comment\n").unwrap(), Vec::new());
    }

    #[test]
    fn valid_sql_returns_no_findings() {
        assert_eq!(lint_sql("SELECT 1").unwrap(), Vec::new());
    }

    #[test]
    fn invalid_sql_is_parse_error() {
        assert!(matches!(lint_sql("ALTER TABLE"), Err(LintError::Parse(_))));
    }

    #[test]
    fn engine_finding_order_and_positional_enrichment() {
        let sql = "CREATE INDEX i ON t (x);\n\nALTER TABLE t ALTER COLUMN a TYPE bigint, ALTER COLUMN a SET NOT NULL;\n";
        let f = lint_sql(sql).unwrap();

        // Order: statement source order, then registry order within a statement.
        // set-not-null is registered before alter-column-type, so it comes first.
        let ids: Vec<&str> = f.iter().map(|x| x.rule_id.as_str()).collect();
        assert_eq!(
            ids,
            ["non-concurrent-index", "set-not-null", "alter-column-type"]
        );

        assert_eq!(f[0].statement_index, 0);
        assert_eq!(f[1].statement_index, 1);
        assert_eq!(f[2].statement_index, 1);

        // location.byte points at the statement's first real token (NOT leading whitespace):
        for finding in &f {
            let b = finding.location.byte as usize;
            assert!(
                sql[b..].starts_with(&finding.snippet),
                "location.byte must point at the trimmed statement start"
            );
        }

        // line/column: CREATE on line 1, the ALTER on line 3 (after the blank line).
        assert_eq!((f[0].location.line, f[0].location.column), (1, 1));
        assert_eq!((f[1].location.line, f[1].location.column), (3, 1));
        assert_eq!(f[2].location.line, 3);

        assert!(f[1].snippet.starts_with("ALTER TABLE"));
    }

    #[test]
    fn findings_json_round_trip() {
        let findings = lint_sql("CREATE INDEX i ON t (x)").unwrap();
        let json = serde_json::to_string(&findings).unwrap();
        let back: Vec<Finding> = serde_json::from_str(&json).unwrap();
        assert_eq!(findings, back);
    }
}
