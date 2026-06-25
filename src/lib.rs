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

    /// Verifies the engine's determinism and positional-enrichment contract across a
    /// two-statement input:
    ///
    /// 1. `statement_index` is 0-based source order, assigned per-statement.
    /// 2. Within a single statement that triggers multiple rules, findings appear in
    ///    `all_rules()` registry order — here `set-not-null` (pos 3) before
    ///    `alter-column-type` (pos 4) even though the TYPE command comes first in the SQL.
    /// 3. `location` is the statement's byte offset in the original SQL input.
    /// 4. `snippet` is the trimmed source text of the statement, and `sql[location..]`
    ///    starts with that snippet (i.e. location indexes into the source).
    #[test]
    fn engine_finding_order_and_positional_enrichment() {
        // Statement 0 (offset 0): non-concurrent index — one finding.
        // Statement 1 (offset 24): two ALTER TABLE commands — two findings in registry order.
        let sql = "CREATE INDEX i ON t (x);\
                   ALTER TABLE t ALTER COLUMN a TYPE bigint, ALTER COLUMN a SET NOT NULL";

        let findings = lint_sql(sql).unwrap();

        assert_eq!(findings.len(), 3, "expected exactly 3 findings");

        // --- statement_index is correct and in source order ---
        assert_eq!(findings[0].statement_index, 0, "stmt 0 finding has wrong index");
        assert_eq!(findings[1].statement_index, 1, "stmt 1 first finding has wrong index");
        assert_eq!(findings[2].statement_index, 1, "stmt 1 second finding has wrong index");

        // --- rule order: source order across statements, registry order within a statement ---
        // non-concurrent-index (stmt 0), then set-not-null before alter-column-type (stmt 1)
        assert_eq!(findings[0].rule_id, "non-concurrent-index");
        assert_eq!(findings[1].rule_id, "set-not-null", "set-not-null must precede alter-column-type (registry order)");
        assert_eq!(findings[2].rule_id, "alter-column-type");

        // --- snippet is the trimmed source text of each statement ---
        assert_eq!(findings[0].snippet, "CREATE INDEX i ON t (x)");
        let expected_stmt1 =
            "ALTER TABLE t ALTER COLUMN a TYPE bigint, ALTER COLUMN a SET NOT NULL";
        assert_eq!(findings[1].snippet, expected_stmt1);
        assert_eq!(findings[2].snippet, expected_stmt1, "both findings share the same statement");

        // --- location is the byte offset of the statement in the input ---
        let stmt0_start = 0_i32;
        let stmt1_start = sql.find("ALTER TABLE").unwrap() as i32;
        assert_eq!(findings[0].location, stmt0_start);
        assert_eq!(findings[1].location, stmt1_start);
        assert_eq!(findings[2].location, stmt1_start);

        // --- sql[location..] starts with the snippet (location indexes into the source) ---
        for f in &findings {
            let loc = f.location as usize;
            assert_eq!(
                &sql[loc..loc + f.snippet.len()],
                f.snippet.as_str(),
                "snippet at location does not match source text for rule {}",
                f.rule_id,
            );
        }
    }
}
