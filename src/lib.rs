//! pgsafe — static safety linter for PostgreSQL DDL migrations.

mod rules;

/// Severity of a finding.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Error,
}

/// Source position of a finding's statement: byte offset plus 1-based line and
/// (character) column of the statement's first non-whitespace token.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Location {
    pub byte: u32,
    pub line: u32,
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

/// A finding reported for a specific statement in the input.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    pub rule_id: String,
    pub severity: Severity,
    pub message: String,
    pub guidance: String,
    pub statement_index: usize,
    pub location: Location,
    pub snippet: String,
}

/// Error returned when the input SQL cannot be parsed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LintError {
    #[error("parse error: {0}")]
    Parse(String),
}

struct StatementSpan {
    location: Location,
    snippet: String,
}

/// Compute the trimmed snippet AND the corrected location (pointing at the
/// first non-whitespace byte of the statement, not the leading whitespace).
fn statement_span(sql: &str, raw: &pg_query::protobuf::RawStmt) -> StatementSpan {
    let off = raw.stmt_location.max(0) as usize;
    let len = raw.stmt_len.max(0) as usize;
    let end = if len == 0 {
        sql.len()
    } else {
        off.saturating_add(len).min(sql.len())
    };
    let raw_slice = sql.get(off..end).unwrap_or("");
    let lead_ws = raw_slice.len() - raw_slice.trim_start().len();
    let start = off + lead_ws;
    let snippet = raw_slice.trim().to_string();
    let (line, column) = line_col(sql, start);
    StatementSpan {
        location: Location {
            byte: start as u32,
            line,
            column,
        },
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

/// Lint one or more SQL statements against all enabled rules.
pub fn lint_sql(sql: &str) -> Result<Vec<Finding>, LintError> {
    let parsed = pg_query::parse(sql).map_err(|e| LintError::Parse(e.to_string()))?;
    let rules = rules::all_rules();
    let mut findings = Vec::new();

    for (i, raw) in parsed.protobuf.stmts.iter().enumerate() {
        let Some(stmt_box) = raw.stmt.as_ref() else {
            continue;
        };
        let Some(node) = stmt_box.node.as_ref() else {
            continue;
        };

        let span = statement_span(sql, raw);
        let mut hits = Vec::new();
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
}
