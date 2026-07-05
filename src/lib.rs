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

mod fix;
mod output;
mod rules;
mod sarif;
mod suppression;
mod synthesized;

// `known_rule_ids` names each synthesized lint's `ID`; bring those submodules into
// crate-root scope so it refers to them unqualified (`timeout::ID`, …), the same as the
// registered rules in `rules`. The engine dispatch itself lives in `synthesized::run_all`.
use synthesized::{
    do_block, enum_value, fk_index, forbid_nullable_fk, forbidden_types, identifier, naming,
    require_columns, require_comment, require_if_exists, require_not_null, require_pk, timeout,
    txn,
};

#[cfg(feature = "cli")]
pub mod cli;

pub use output::{
    gate, lint_input, render_errors, render_finding_body, render_finding_human, render_github,
    render_human, render_json, render_statement_header, FailOn, FileReport, Format, SCHEMA_VERSION,
};
pub use sarif::render_sarif;

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
    pub fix: Option<crate::fix::FixDraft>,
}

/// A single text edit within the linted SQL: replace bytes `[start, end)` with
/// `replacement`. `start == end` is a pure insertion.
///
/// All offsets are 0-based byte positions into the linted SQL string and are
/// guaranteed to fall on UTF-8 character boundaries within the range of the
/// linted input.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FixEdit {
    /// 0-based byte offset where the edit begins.
    pub start: u32,
    /// 0-based byte offset where the edit ends (`== start` for an insertion).
    pub end: u32,
    /// Text to splice in place of `[start, end)`.
    pub replacement: String,
}

/// A machine-applicable remediation for a [`Finding`]: a short title plus the
/// edits that make the statement safe. Present only for findings whose fix is an
/// unambiguous mechanical change.
///
/// # Guarantees (upheld by construction)
///
/// A consumer can rely on the following invariants:
///
/// - **Non-empty**: `edits` contains at least one edit.
/// - **Ascending order**: edits are sorted by `start` in ascending order.
/// - **Non-overlapping**: for each consecutive pair, `prev.end <= next.start`.
/// - **In-range**: every `start` and `end` offset falls within the byte length
///   of the linted SQL string.
/// - **UTF-8 boundaries**: offsets land on UTF-8 character boundaries so that
///   splicing via `str::replace_range` does not panic.
///
/// Apply edits **high-to-low** (sort descending by `start` before splicing) so
/// that each splice does not shift the byte offsets of earlier edits.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Fix {
    /// Short label for the fix, e.g. `"Add CONCURRENTLY"`.
    pub title: String,
    /// The edits to apply, in ascending `start` order; non-overlapping; at least one.
    pub edits: Vec<FixEdit>,
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
    /// `Some` when this finding has a safe, machine-applicable fix.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fix: Option<Fix>,
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
    /// `forbid-nullable-fk`, `unchecked-do-block`) to run; has no effect on rules that are on by
    /// default. The **data-configured** policies (`naming-convention`, `forbidden-column-type`,
    /// `require-columns`) activate when their own field below is non-empty, independent of this set.
    /// Default empty.
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
    /// Column names every `CREATE TABLE` must include. Matched case-insensitively: the rule folds
    /// each name to lower case to match PostgreSQL's unquoted-identifier folding, so any casing here
    /// works (`Created_At` matches a `created_at` column). A quoted, mixed-case column keeps its case
    /// and is not matched. The `require-columns` rule runs only when this is non-empty. Default empty.
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
/// `unchecked-do-block`, `require-comment`, `require-columns`, `forbid-nullable-fk`).
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
    ids.push(do_block::ID);
    ids.push(require_comment::ID);
    ids.push(require_columns::ID);
    ids.push(forbid_nullable_fk::ID);
    ids
}

/// Every lint-rule id this build can emit — the registered AST rules plus the
/// engine-synthesized/policy rules — in stable order. The public rule catalog,
/// e.g. for `pgsafe --list-rules` and external tooling.
#[must_use]
pub fn list_rule_ids() -> Vec<&'static str> {
    known_rule_ids()
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
                let fix = h.fix.as_ref().and_then(|d| {
                    let r = crate::fix::resolve(d, sql, g.start, g.end);
                    debug_assert!(
                        r.is_some() || d.may_legitimately_not_resolve(),
                        "rule {}: fix draft {:?} failed to resolve",
                        rule.id(),
                        d.title
                    );
                    r
                });
                findings.push(Finding {
                    rule_id: rule.id().to_string(),
                    severity: rule.severity(),
                    message: h.message,
                    guidance: h.guidance,
                    statement_index: i,
                    location,
                    snippet: snippet.clone(),
                    suppression: None,
                    fix,
                });
            }
        }
    }
    let (mut findings, new_table_dropped) =
        synthesized::run_all(sql, stmts, &geoms, options, findings);
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

    #[test]
    fn hazard_inside_do_block_is_flagged_by_the_real_rule() {
        let f = lint_sql(
            "DO $$ BEGIN CREATE INDEX i ON t (c); END $$;",
            &LintOptions::default(),
        )
        .unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "add-index-non-concurrent")
            .expect("a non-CONCURRENTLY CREATE INDEX in a DO block must be flagged");
        assert!(hit.message.starts_with("Inside a DO block:"));
        assert!(hit.snippet.contains("CREATE INDEX"));
    }

    #[test]
    fn safe_do_block_produces_no_findings() {
        let f = lint_sql("DO $$ BEGIN PERFORM 1; END $$;", &LintOptions::default()).unwrap();
        assert!(f.is_empty());
    }

    #[test]
    fn multiple_hazards_in_one_do_block_each_flagged() {
        let f = lint_sql(
            "DO $$ BEGIN CREATE INDEX i ON t (c); DROP TABLE u; END $$;",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(f.iter().any(|f| f.rule_id == "add-index-non-concurrent"));
        assert!(f.iter().any(|f| f.rule_id == "drop-table"));
    }

    #[test]
    fn embedded_finding_respects_disabled_rules() {
        let opts = LintOptions {
            disabled_rules: ["add-index-non-concurrent".to_string()]
                .into_iter()
                .collect(),
            ..LintOptions::default()
        };
        let f = lint_sql("DO $$ BEGIN CREATE INDEX i ON t (c); END $$;", &opts).unwrap();
        assert!(f.iter().all(|f| f.rule_id != "add-index-non-concurrent"));
    }

    #[test]
    fn embedded_finding_is_suppressible_on_the_do_statement() {
        let sql = "-- pgsafe:ignore add-index-non-concurrent reviewed\n\
                   DO $$ BEGIN CREATE INDEX i ON t (c); END $$;";
        let f = lint_sql(sql, &LintOptions::default()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "add-index-non-concurrent")
            .expect("finding must still be present, just suppressed");
        assert!(hit.is_suppressed());
    }

    #[test]
    fn mixed_do_block_yields_both_embedded_and_residue_findings() {
        let opts = LintOptions {
            enabled_rules: ["unchecked-do-block".to_string()].into_iter().collect(),
            ..LintOptions::default()
        };
        let f = lint_sql(
            "DO $$ BEGIN CREATE INDEX i ON t (c); EXECUTE 'DROP TABLE u'; END $$;",
            &opts,
        )
        .unwrap();
        assert!(f.iter().any(|f| f.rule_id == "add-index-non-concurrent"
            && f.message.starts_with("Inside a DO block:")));
        assert!(f.iter().any(|f| f.rule_id == "unchecked-do-block"));
    }

    #[test]
    fn finding_serializes_fix_when_present() {
        let f = Finding {
            rule_id: "add-index-non-concurrent".into(),
            severity: Severity::Error,
            message: "m".into(),
            guidance: "g".into(),
            statement_index: 0,
            location: Location {
                byte: 0,
                line: 1,
                column: 1,
            },
            snippet: "CREATE INDEX i ON t (c)".into(),
            suppression: None,
            fix: Some(Fix {
                title: "Add CONCURRENTLY".into(),
                edits: vec![FixEdit {
                    start: 12,
                    end: 12,
                    replacement: " CONCURRENTLY".into(),
                }],
            }),
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(
            json.contains(r#""fix":{"title":"Add CONCURRENTLY""#),
            "{json}"
        );
        assert!(
            json.contains(r#""edits":[{"start":12,"end":12,"replacement":" CONCURRENTLY"}]"#),
            "{json}"
        );
        // Round-trips back to an equal value.
        assert_eq!(serde_json::from_str::<Finding>(&json).unwrap(), f);
    }

    #[test]
    fn finding_omits_fix_when_absent() {
        let f = Finding {
            rule_id: "drop-column".into(),
            severity: Severity::Warning,
            message: "m".into(),
            guidance: "g".into(),
            statement_index: 0,
            location: Location {
                byte: 0,
                line: 1,
                column: 1,
            },
            snippet: "ALTER TABLE t DROP COLUMN c".into(),
            suppression: None,
            fix: None,
        };
        assert!(!serde_json::to_string(&f).unwrap().contains("fix"));
    }

    #[test]
    fn rule_hit_default_carries_no_fix() {
        // A rule that emits no fix still produces a finding with fix == None.
        let fs = lint_sql("DROP TABLE t;", &LintOptions::default()).unwrap();
        let f = fs.iter().find(|f| f.rule_id == "drop-table").unwrap();
        assert!(f.fix.is_none());
    }
}
