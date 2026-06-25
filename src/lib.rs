//! pgsafe — static safety linter for PostgreSQL DDL migrations.

/// Severity of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// What a rule emits when it matches. The engine adds positional context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleHit {
    pub rule_id: &'static str,
    pub severity: Severity,
    pub message: String,
    pub guidance: String,
}

/// A finding reported for a specific statement in the input.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    pub rule_id: String,
    pub severity: Severity,
    pub message: String,
    pub guidance: String,
    pub statement_index: usize,
    pub location: i32,
    pub snippet: String,
}

/// Error returned when the input SQL cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LintError {
    Parse(String),
}

impl std::fmt::Display for LintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LintError::Parse(msg) => write!(f, "parse error: {msg}"),
        }
    }
}

impl std::error::Error for LintError {}

/// Lint one or more SQL statements. v0 scaffold: parses only, no rules yet.
pub fn lint_sql(sql: &str) -> Result<Vec<Finding>, LintError> {
    pg_query::parse(sql).map_err(|e| LintError::Parse(e.to_string()))?;
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_sql_returns_no_findings() {
        assert_eq!(lint_sql("SELECT 1").unwrap(), Vec::new());
    }

    #[test]
    fn invalid_sql_is_parse_error() {
        assert!(matches!(lint_sql("ALTER TABLE"), Err(LintError::Parse(_))));
    }
}
