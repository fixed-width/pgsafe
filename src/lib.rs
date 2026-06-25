//! pgsafe — static safety linter for PostgreSQL DDL migrations.

pub mod rules;

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

/// Extract the trimmed source text of a single parsed statement.
fn statement_text(sql: &str, raw: &pg_query::protobuf::RawStmt) -> String {
    let off = raw.stmt_location.max(0) as usize;
    let end = if raw.stmt_len == 0 {
        sql.len()
    } else {
        (off + raw.stmt_len as usize).min(sql.len())
    };
    sql.get(off..end).unwrap_or("").trim().to_string()
}

/// Lint one or more SQL statements against all enabled rules.
pub fn lint_sql(sql: &str) -> Result<Vec<Finding>, LintError> {
    let parsed = pg_query::parse(sql).map_err(|e| LintError::Parse(e.to_string()))?;
    let rules = rules::all_rules();
    let mut findings = Vec::new();

    for (i, raw) in parsed.protobuf.stmts.iter().enumerate() {
        let Some(stmt_box) = raw.stmt.as_ref() else { continue };
        let Some(node) = stmt_box.node.as_ref() else { continue };

        let mut hits = Vec::new();
        for rule in &rules {
            rule.check(node, &mut hits);
        }
        if hits.is_empty() {
            continue;
        }

        let snippet = statement_text(sql, raw);
        for h in hits {
            findings.push(Finding {
                rule_id: h.rule_id.to_string(),
                severity: h.severity,
                message: h.message,
                guidance: h.guidance,
                statement_index: i,
                location: raw.stmt_location,
                snippet: snippet.clone(),
            });
        }
    }

    Ok(findings)
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
