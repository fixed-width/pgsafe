//! `pgsafe` is a static safety linter for PostgreSQL DDL migrations.
//!
//! It parses SQL using the real PostgreSQL parser and checks every statement
//! against a set of rules that flag lock-taking or destructive operations,
//! producing a [`Finding`] for each match together with safe-rewrite guidance.
//!
//! The main entry point is [`lint_sql`]. A command-line interface wrapping
//! the same rules is provided by the `pgsafe` binary.
#![deny(missing_docs)]

mod newtable;
mod output;
mod rules;
mod suppression;
mod txn;

pub use output::{gate, FailOn};

/// Severity level of a [`Finding`], ordered by increasing severity
/// (`Warning` < `Error`).
#[non_exhaustive]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
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
    /// 0-based byte offset of the statement's first non-whitespace token within the input.
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

/// Why a [`Finding`] was suppressed by an inline `-- pgsafe:ignore` directive.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Suppression {
    /// The reason text supplied in the directive.
    pub reason: String,
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
    /// Identifier of the rule that triggered this finding (e.g. `"add-index-non-concurrent"`).
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
    /// `Some` when an inline `-- pgsafe:ignore` directive suppressed this finding.
    /// A suppressed finding is still reported but does not affect the gate exit code.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub suppression: Option<Suppression>,
}

impl Finding {
    /// Whether an inline directive suppressed this finding.
    #[must_use]
    pub fn is_suppressed(&self) -> bool {
        self.suppression.is_some()
    }
}

/// Error returned when `pgsafe` cannot process the provided SQL.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LintError {
    /// The SQL input could not be parsed by the PostgreSQL parser.
    #[error("parse error: {0}")]
    Parse(String),
}

/// 1-based line and character-column of a byte offset within `sql`.
pub(crate) fn line_col(sql: &str, byte: usize) -> (u32, u32) {
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
    let parsed = pg_query::parse(sql).map_err(|e| LintError::Parse(e.to_string()))?;
    let stmts = &parsed.protobuf.stmts;
    let comments = suppression::scan_comments(sql)?;
    let geoms = suppression::geometry(sql, stmts, &comments);
    let rules = rules::all_rules();
    let mut findings = Vec::new();
    let mut hits = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(stmt_box) = raw.stmt.as_ref() else {
            continue;
        };
        let Some(node) = stmt_box.node.as_ref() else {
            continue;
        };
        let g = &geoms[i];
        let (line, column) = line_col(sql, g.start);
        let location = Location {
            byte: u32::try_from(g.start).unwrap_or(u32::MAX),
            line,
            column,
        };
        let snippet = sql.get(g.start..g.end).unwrap_or("").trim().to_string();
        for rule in rules {
            rule.check(node, &mut hits);
            for h in hits.drain(..) {
                findings.push(Finding {
                    rule_id: rule.id().to_string(),
                    severity: rule.severity(),
                    message: h.message,
                    guidance: h.guidance,
                    statement_index: i,
                    location,
                    snippet: snippet.clone(),
                    suppression: None,
                });
            }
        }
    }
    let (mut findings, new_table_dropped) = newtable::drop_new_table_findings(stmts, findings);
    for i in txn::concurrently_in_transaction_indices(stmts) {
        let g = &geoms[i];
        let (line, column) = line_col(sql, g.start);
        findings.push(Finding {
            rule_id: txn::ID.to_string(),
            severity: Severity::Error,
            message: txn::MESSAGE.to_string(),
            guidance: txn::GUIDANCE.to_string(),
            statement_index: i,
            location: Location {
                byte: u32::try_from(g.start).unwrap_or(u32::MAX),
                line,
                column,
            },
            snippet: sql.get(g.start..g.end).unwrap_or("").trim().to_string(),
            suppression: None,
        });
    }
    let mut known_ids = rules::rule_ids();
    known_ids.push(txn::ID);
    suppression::resolve(
        sql,
        &geoms,
        &comments,
        findings,
        &known_ids,
        &new_table_dropped,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_is_ordered_warning_below_error() {
        assert!(Severity::Warning < Severity::Error);
    }

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
            [
                "add-index-non-concurrent",
                "set-not-null",
                "alter-column-type"
            ]
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

    #[test]
    fn suppression_field_defaults_to_none_and_is_omitted_from_json() {
        let f = &lint_sql("CREATE INDEX i ON t (x)").unwrap()[0];
        assert!(!f.is_suppressed());
        let json = serde_json::to_string(f).unwrap();
        assert!(
            !json.contains("suppression"),
            "None suppression must be omitted"
        );
    }

    #[test]
    fn suppression_round_trips_when_present() {
        let mut f = lint_sql("CREATE INDEX i ON t (x)").unwrap().remove(0);
        f.suppression = Some(Suppression {
            reason: "off-peak deploy".into(),
        });
        assert!(f.is_suppressed());
        let back: Finding = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(back.suppression.unwrap().reason, "off-peak deploy");
    }
}
