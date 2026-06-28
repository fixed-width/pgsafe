//! `pgsafe` is a static safety linter for PostgreSQL DDL migrations.
//!
//! It parses SQL using the real PostgreSQL parser and checks every statement
//! against a set of rules that flag lock-taking or destructive operations,
//! producing a [`Finding`] for each match together with safe-rewrite guidance.
//!
//! The main entry point is [`lint_sql`]. A command-line interface wrapping
//! the same rules is provided by the `pgsafe` binary.
#![deny(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};

mod enum_value;
mod fk_index;
mod forbid_nullable_fk;
mod forbidden_types;
mod identifier;
mod naming;
mod newtable;
mod output;
mod require_columns;
mod require_comment;
mod require_if_exists;
mod require_not_null;
mod require_pk;
mod rules;
mod suppression;
mod timeout;
mod txn;

#[cfg(feature = "cli")]
pub mod cli;

#[cfg(feature = "cli")]
mod config;

#[cfg(feature = "cli")]
mod gitdiff;

pub use output::{
    gate, lint_input, render_errors, render_finding_human, render_human, render_json, FailOn,
    FileReport, Format, SCHEMA_VERSION,
};

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

/// The kind of identifier a naming-convention pattern applies to.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NameKind {
    /// A table.
    Table,
    /// A column.
    Column,
    /// An index.
    Index,
    /// A constraint.
    Constraint,
    /// A sequence.
    Sequence,
    /// A trigger.
    Trigger,
    /// A schema.
    Schema,
}

impl NameKind {
    /// The lowercase config-key / display name (`"table"`, `"column"`, …).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            NameKind::Table => "table",
            NameKind::Column => "column",
            NameKind::Index => "index",
            NameKind::Constraint => "constraint",
            NameKind::Sequence => "sequence",
            NameKind::Trigger => "trigger",
            NameKind::Schema => "schema",
        }
    }
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

/// Options that adjust linting beyond the default static analysis.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct LintOptions {
    /// Treat the input as already running inside a transaction (e.g. a migration tool that wraps
    /// each migration implicitly), so `CONCURRENTLY` index operations are flagged even without an
    /// explicit `BEGIN`. Default `false`.
    pub assume_in_transaction: bool,
    /// Rule ids that must not run for this input (their findings — and, for synthesized rules,
    /// their syntheses — are skipped). Default empty.
    pub disabled_rules: BTreeSet<String>,
    /// Rule ids explicitly enabled in config. Required for the **boolean** opt-in policy rules
    /// (`require-primary-key`, `require-not-null`, `require-if-exists`, `require-comment`,
    /// `forbid-nullable-fk`) to run; has no effect on rules that are on by default. The
    /// **data-configured** policies (`naming-convention`, `forbidden-column-type`, `require-columns`)
    /// activate when their own field below is non-empty, independent of this set. Default empty.
    pub enabled_rules: BTreeSet<String>,
    /// Per-rule severity overrides applied to the findings this run emits, keyed by rule id.
    /// Default empty.
    pub severity_overrides: BTreeMap<String, Severity>,
    /// Per-kind naming-convention patterns (raw regex strings). The `naming-convention` rule runs only
    /// when this is non-empty. Default empty.
    pub naming_patterns: BTreeMap<NameKind, String>,
    /// Forbidden column types mapped to a suggested replacement (raw spellings). The
    /// `forbidden-column-type` rule runs only when this is non-empty. Default empty.
    pub forbidden_column_types: BTreeMap<String, String>,
    /// Column names every `CREATE TABLE` must include, compared against the parsed column name. Use
    /// lower case to match PostgreSQL's unquoted-identifier folding (the CLI lowercases config
    /// values); a quoted, mixed-case column keeps its case and is not matched. The `require-columns`
    /// rule runs only when this is non-empty. Default empty.
    pub required_columns: BTreeSet<String>,
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

/// Every lint-rule id: the registered rules plus the engine-synthesized ones, in push order
/// (`concurrently-in-transaction`, `require-timeout`, `identifier-too-long`,
/// `fk-without-covering-index`, `enum-value-used-in-transaction`, `require-primary-key`,
/// `require-not-null`, `naming-convention`, `forbidden-column-type`, `require-if-exists`,
/// `require-comment`, `require-columns`, `forbid-nullable-fk`).
/// NOT the `suppression-*` hygiene ids.
pub(crate) fn known_rule_ids() -> Vec<&'static str> {
    let mut ids = rules::rule_ids();
    ids.push(txn::ID);
    ids.push(timeout::ID);
    ids.push(identifier::ID);
    ids.push(fk_index::ID);
    ids.push(enum_value::ID);
    ids.push(require_pk::ID);
    ids.push(require_not_null::ID);
    ids.push(naming::ID);
    ids.push(forbidden_types::ID);
    ids.push(require_if_exists::ID);
    ids.push(require_comment::ID);
    ids.push(require_columns::ID);
    ids.push(forbid_nullable_fk::ID);
    ids
}

/// Push one engine-synthesized rule's hits as [`Finding`]s. Each hit is a `(statement_index, message,
/// guidance)` tuple — the statement index sources the location and snippet, and the message/guidance
/// are this finding's own. `rule_id` and `severity` are constant for the rule. Centralizes the
/// location / snippet / `Finding` construction shared by every synthesized block in [`lint_sql`].
fn push_synthesized(
    findings: &mut Vec<Finding>,
    sql: &str,
    geoms: &[suppression::StatementGeom],
    rule_id: &str,
    severity: Severity,
    hits: impl IntoIterator<Item = (usize, String, String)>,
) {
    for (i, message, guidance) in hits {
        // The index comes from a rule walking these same `stmts`, so it is always in range; assert
        // it in debug builds to attribute any future rule bug to its source rather than this push.
        debug_assert!(
            i < geoms.len(),
            "synthesized hit index {i} out of range for {rule_id}"
        );
        let g = &geoms[i];
        let (line, column) = line_col(sql, g.start);
        findings.push(Finding {
            rule_id: rule_id.to_string(),
            severity,
            message,
            guidance,
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
}

/// Lint `sql` under `options`, returning findings in source order.
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
/// let findings = pgsafe::lint_sql(
///     "CREATE INDEX i ON t (x)",
///     &pgsafe::LintOptions::default(),
/// ).unwrap();
/// assert!(!findings.is_empty());
/// ```
pub fn lint_sql(sql: &str, options: &LintOptions) -> Result<Vec<Finding>, LintError> {
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
            if options.disabled_rules.contains(rule.id()) {
                continue;
            }
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
    if !options.disabled_rules.contains(timeout::ID) {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            timeout::ID,
            Severity::Warning,
            timeout::require_timeout_indices(stmts, options.assume_in_transaction)
                .into_iter()
                .map(|i| {
                    (
                        i,
                        timeout::MESSAGE.to_string(),
                        timeout::GUIDANCE.to_string(),
                    )
                }),
        );
    }
    let (mut findings, new_table_dropped) = newtable::drop_new_table_findings(stmts, findings);
    if !options.disabled_rules.contains(txn::ID) {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            txn::ID,
            Severity::Error,
            txn::concurrently_in_transaction_indices(stmts, options.assume_in_transaction)
                .into_iter()
                .map(|i| (i, txn::MESSAGE.to_string(), txn::GUIDANCE.to_string())),
        );
    }
    if !options.disabled_rules.contains(identifier::ID) {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            identifier::ID,
            Severity::Warning,
            identifier::long_identifiers(sql).into_iter().filter_map(|(off, name, len)| {
                let i = geoms.iter().position(|g| off >= g.start && off < g.end)?;
                Some((
                    i,
                    format!(
                        "Identifier `{name}` is {len} bytes; PostgreSQL truncates identifiers to 63 \
                         bytes, so two names sharing a 63-byte prefix silently collide."
                    ),
                    "Shorten the identifier to 63 bytes or fewer so PostgreSQL does not silently \
                     truncate it."
                        .to_string(),
                ))
            }),
        );
    }
    if !options.disabled_rules.contains(fk_index::ID) {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            fk_index::ID,
            Severity::Warning,
            fk_index::fk_without_index(stmts).into_iter().map(|(i, table, col)| {
                (
                    i,
                    format!(
                        "Foreign key on `{table}` column `{col}` has no covering index; referential \
                         checks and ON DELETE/UPDATE actions on the parent scan and lock the child on \
                         every change."
                    ),
                    format!(
                        "Add a covering index on the referencing column, e.g. \
                         `CREATE INDEX CONCURRENTLY ON {table} ({col});`."
                    ),
                )
            }),
        );
    }
    if !options.disabled_rules.contains(enum_value::ID) {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            enum_value::ID,
            Severity::Warning,
            enum_value::unsafe_enum_value_indices(sql, stmts, options.assume_in_transaction)
                .into_iter()
                .map(|i| {
                    (
                        i,
                        enum_value::MESSAGE.to_string(),
                        enum_value::GUIDANCE.to_string(),
                    )
                }),
        );
    }
    if options.enabled_rules.contains(require_pk::ID)
        && !options.disabled_rules.contains(require_pk::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            require_pk::ID,
            Severity::Warning,
            require_pk::tables_without_primary_key(stmts)
                .into_iter()
                .map(|i| {
                    (
                        i,
                        require_pk::MESSAGE.to_string(),
                        require_pk::GUIDANCE.to_string(),
                    )
                }),
        );
    }
    if options.enabled_rules.contains(require_not_null::ID)
        && !options.disabled_rules.contains(require_not_null::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            require_not_null::ID,
            Severity::Warning,
            require_not_null::nullable_columns(stmts)
                .into_iter()
                .map(|(i, message)| (i, message, require_not_null::GUIDANCE.to_string())),
        );
    }
    if !options.naming_patterns.is_empty() && !options.disabled_rules.contains(naming::ID) {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            naming::ID,
            Severity::Warning,
            naming::naming_violations(stmts, &options.naming_patterns)
                .into_iter()
                .map(|(i, message)| (i, message, naming::GUIDANCE.to_string())),
        );
    }
    if !options.forbidden_column_types.is_empty()
        && !options.disabled_rules.contains(forbidden_types::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            forbidden_types::ID,
            Severity::Warning,
            forbidden_types::forbidden_violations(stmts, &options.forbidden_column_types)
                .into_iter()
                .map(|(i, message)| (i, message, forbidden_types::GUIDANCE.to_string())),
        );
    }
    if options.enabled_rules.contains(require_if_exists::ID)
        && !options.disabled_rules.contains(require_if_exists::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            require_if_exists::ID,
            Severity::Warning,
            require_if_exists::missing_if_exists(stmts)
                .into_iter()
                .map(|(i, message)| (i, message, require_if_exists::GUIDANCE.to_string())),
        );
    }
    if options.enabled_rules.contains(require_comment::ID)
        && !options.disabled_rules.contains(require_comment::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            require_comment::ID,
            Severity::Warning,
            require_comment::missing_comments(stmts)
                .into_iter()
                .map(|(i, message)| (i, message, require_comment::GUIDANCE.to_string())),
        );
    }
    if !options.required_columns.is_empty() && !options.disabled_rules.contains(require_columns::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            require_columns::ID,
            Severity::Warning,
            require_columns::missing_required_columns(stmts, &options.required_columns)
                .into_iter()
                .map(|(i, message)| (i, message, require_columns::GUIDANCE.to_string())),
        );
    }
    if options.enabled_rules.contains(forbid_nullable_fk::ID)
        && !options.disabled_rules.contains(forbid_nullable_fk::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            &geoms,
            forbid_nullable_fk::ID,
            Severity::Warning,
            forbid_nullable_fk::nullable_fk_columns(stmts)
                .into_iter()
                .map(|(i, message)| (i, message, forbid_nullable_fk::GUIDANCE.to_string())),
        );
    }
    if !options.severity_overrides.is_empty() {
        for f in &mut findings {
            if let Some(&sev) = options.severity_overrides.get(&f.rule_id) {
                f.severity = sev;
            }
        }
    }
    let known_ids = known_rule_ids();
    suppression::resolve(
        sql,
        &geoms,
        &comments,
        findings,
        &known_ids,
        &new_table_dropped,
        &options.disabled_rules,
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
        assert_eq!(lint_sql("", &LintOptions::default()).unwrap(), Vec::new());
    }

    #[test]
    fn comment_only_returns_no_findings() {
        assert_eq!(
            lint_sql("-- just a comment\n", &LintOptions::default()).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn valid_sql_returns_no_findings() {
        assert_eq!(
            lint_sql("SELECT 1", &LintOptions::default()).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn invalid_sql_is_parse_error() {
        assert!(matches!(
            lint_sql("ALTER TABLE", &LintOptions::default()),
            Err(LintError::Parse(_))
        ));
    }

    #[test]
    fn engine_finding_order_and_positional_enrichment() {
        let sql = "CREATE INDEX i ON t (x);\n\nALTER TABLE t ALTER COLUMN a TYPE bigint, ALTER COLUMN a SET NOT NULL;\n";
        let f = lint_sql(sql, &LintOptions::default()).unwrap();

        // Order: statement source order; within a statement, rule-loop findings
        // (registry order) precede engine-synthesized findings.
        // set-not-null is registered before alter-column-type, so it comes first;
        // require-timeout is synthesized after the rule loop for each statement.
        let ids: Vec<&str> = f.iter().map(|x| x.rule_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "add-index-non-concurrent",
                "require-timeout",
                "set-not-null",
                "alter-column-type",
                "require-timeout",
            ]
        );

        assert_eq!(f[0].statement_index, 0);
        assert_eq!(f[1].statement_index, 0);
        assert_eq!(f[2].statement_index, 1);
        assert_eq!(f[3].statement_index, 1);
        assert_eq!(f[4].statement_index, 1);

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
        assert_eq!((f[2].location.line, f[2].location.column), (3, 1));
        assert_eq!(f[3].location.line, 3);

        assert!(f[2].snippet.starts_with("ALTER TABLE"));
    }

    #[test]
    fn findings_json_round_trip() {
        let findings = lint_sql("CREATE INDEX i ON t (x)", &LintOptions::default()).unwrap();
        let json = serde_json::to_string(&findings).unwrap();
        let back: Vec<Finding> = serde_json::from_str(&json).unwrap();
        assert_eq!(findings, back);
    }

    #[test]
    fn suppression_field_defaults_to_none_and_is_omitted_from_json() {
        let f = &lint_sql("CREATE INDEX i ON t (x)", &LintOptions::default()).unwrap()[0];
        assert!(!f.is_suppressed());
        let json = serde_json::to_string(f).unwrap();
        assert!(
            !json.contains("suppression"),
            "None suppression must be omitted"
        );
    }

    #[test]
    fn suppression_round_trips_when_present() {
        let mut f = lint_sql("CREATE INDEX i ON t (x)", &LintOptions::default())
            .unwrap()
            .remove(0);
        f.suppression = Some(Suppression {
            reason: "off-peak deploy".into(),
        });
        assert!(f.is_suppressed());
        let back: Finding = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(back.suppression.unwrap().reason, "off-peak deploy");
    }

    #[test]
    fn assume_in_transaction_flags_top_level_concurrently() {
        let sql = "CREATE INDEX CONCURRENTLY i ON t (x)";
        let on = lint_sql(
            sql,
            &LintOptions {
                assume_in_transaction: true,
                ..LintOptions::default()
            },
        )
        .unwrap();
        assert!(on
            .iter()
            .any(|f| f.rule_id == "concurrently-in-transaction"));
        let off = lint_sql(sql, &LintOptions::default()).unwrap();
        assert!(!off
            .iter()
            .any(|f| f.rule_id == "concurrently-in-transaction"));
    }

    fn opts(disabled: &[&str], overrides: &[(&str, Severity)]) -> LintOptions {
        LintOptions {
            disabled_rules: disabled.iter().map(|s| (*s).to_string()).collect(),
            severity_overrides: overrides
                .iter()
                .map(|(k, v)| ((*k).to_string(), *v))
                .collect(),
            ..LintOptions::default()
        }
    }

    #[test]
    fn disabled_registered_rule_produces_no_finding() {
        let fs = lint_sql("DROP TABLE x;", &opts(&["drop-table"], &[])).unwrap();
        assert!(!fs.iter().any(|f| f.rule_id == "drop-table"));
    }

    #[test]
    fn disabled_synthesized_rule_is_silent() {
        // require-timeout normally fires on a bare ALTER TABLE.
        let fs = lint_sql(
            "ALTER TABLE t ADD COLUMN c int;",
            &opts(&["require-timeout"], &[]),
        )
        .unwrap();
        assert!(!fs.iter().any(|f| f.rule_id == "require-timeout"));
    }

    #[test]
    fn severity_override_changes_a_findings_severity() {
        let fs = lint_sql(
            "CREATE INDEX i ON t (x);",
            &opts(&[], &[("add-index-non-concurrent", Severity::Warning)]),
        )
        .unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "add-index-non-concurrent")
            .unwrap();
        assert_eq!(f.severity, Severity::Warning); // default is Error
    }

    #[test]
    fn directive_for_a_disabled_rule_is_not_reported_unused() {
        let sql = "-- pgsafe:ignore drop-table  disabled in config anyway\nDROP TABLE x;";
        let fs = lint_sql(sql, &opts(&["drop-table"], &[])).unwrap();
        assert!(!fs.iter().any(|f| f.rule_id == "suppression-unused"));
        assert!(!fs.iter().any(|f| f.rule_id == "drop-table"));
    }
}
