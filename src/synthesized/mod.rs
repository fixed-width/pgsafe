//! Engine-synthesized lints: the checks that are **not** registered `Rule` trait
//! implementations (those live in [`crate::rules`]).
//!
//! Each submodule here exposes an `ID` plus `MESSAGE`/`GUIDANCE` constants and a
//! walker function over the parsed statement list, and is dispatched by [`run_all`]
//! rather than through the per-statement rule loop. Two shapes live here:
//!
//! - **Always-on cross-statement checks** — [`txn`], [`timeout`], [`identifier`],
//!   [`fk_index`], [`enum_value`]. These reason across the whole migration and run
//!   unless explicitly disabled.
//! - **Policy lints** (opt-in or config-gated) — [`require_pk`], [`require_not_null`],
//!   [`require_if_exists`], [`require_comment`], [`forbid_nullable_fk`], [`do_block`],
//!   [`naming`], [`forbidden_types`], [`require_columns`], [`require_schema_qualified`].
//!
//! Two shared helpers support the family: [`newtable`] (drops findings on tables the
//! migration itself creates empty, and supplies the `rangevar_key`/`lintable_create_relation`
//! utilities the policy lints reuse) and [`plpgsql`] (recovers analyzable SQL from a
//! `DO` block's PL/pgSQL body).

use std::collections::BTreeSet;

use crate::ast::protobuf::RawStmt;
use crate::ast::NodeEnum;

use crate::line_col;
use crate::suppression::StatementGeom;
use crate::{Finding, LintOptions, Location, Severity};

pub(crate) mod do_block;
pub(crate) mod enum_value;
pub(crate) mod fk_index;
pub(crate) mod forbid_nullable_fk;
pub(crate) mod forbidden_types;
pub(crate) mod identifier;
pub(crate) mod naming;
pub(crate) mod newtable;
pub(crate) mod plpgsql;
pub(crate) mod require_columns;
pub(crate) mod require_comment;
pub(crate) mod require_if_exists;
pub(crate) mod require_not_null;
pub(crate) mod require_pk;
pub(crate) mod require_schema_qualified;
pub(crate) mod timeout;
pub(crate) mod txn;

/// Push one engine-synthesized rule's hits as [`Finding`]s. Each hit is a
/// `(statement_index, message, guidance, fix_draft)` tuple — the statement index sources the
/// location and snippet, the message/guidance are this finding's own, and the optional draft is
/// resolved to an absolute [`crate::Fix`] (or `None` when no fix is provided). `rule_id` and
/// `severity` are constant for the rule. Centralizes the location / snippet / `Finding`
/// construction shared by every synthesized block in [`run_all`].
fn push_synthesized(
    findings: &mut Vec<Finding>,
    sql: &str,
    geoms: &[StatementGeom],
    rule_id: &str,
    severity: Severity,
    hits: impl IntoIterator<Item = (usize, String, String, Option<crate::fix::FixDraft>)>,
) {
    for (i, message, guidance, draft) in hits {
        // The index comes from a rule walking these same `stmts`, so it is always in range; assert
        // it in debug builds to attribute any future rule bug to its source rather than this push.
        debug_assert!(
            i < geoms.len(),
            "synthesized hit index {i} out of range for {rule_id}"
        );
        let g = &geoms[i];
        let (line, column) = line_col(sql, g.start);
        let fix = draft.as_ref().and_then(|d| {
            let r = crate::fix::resolve(d, sql, g.start, g.end);
            debug_assert!(
                r.is_some() || d.may_legitimately_not_resolve(),
                "rule {rule_id}: fix draft {:?} failed to resolve",
                d.title
            );
            r
        });
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
            fix,
        });
    }
}

/// Run every engine-synthesized lint over the already-parsed migration and append their findings
/// to `findings` (which arrives carrying the registered-rule findings from [`crate::lint_sql`]).
///
/// Findings are pushed in the engine's stable order: DO-block-embedded rule findings, then the
/// always-on cross-statement checks, then the opt-in/config-gated policy lints. The returned
/// `BTreeSet` is `new_table_dropped` — the statement indices whose findings were dropped as
/// operations on a table this migration creates empty — which the caller forwards to suppression
/// resolution.
pub(crate) fn run_all(
    sql: &str,
    stmts: &[RawStmt],
    geoms: &[StatementGeom],
    options: &LintOptions,
    mut findings: Vec<Finding>,
) -> (Vec<Finding>, BTreeSet<usize>) {
    let rules = crate::rules::all_rules();
    let mut hits = Vec::new();
    // Lint the static SQL recovered from each DO block's PL/pgSQL body (on by default). Each recovered
    // statement runs through the same per-statement registered rules; findings are attributed to the
    // DO statement (so location and suppression align) and prefixed to mark their origin.
    // Accumulate residue indices: DO blocks with un-analyzable SQL (dynamic EXECUTE or unparsable body).
    let mut residue_do_indices: Vec<usize> = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        if !matches!(node, NodeEnum::DoStmt(_)) {
            continue;
        }
        let g = &geoms[i];
        let (line, column) = line_col(sql, g.start);
        let location = Location {
            byte: u32::try_from(g.start).unwrap_or(u32::MAX),
            line,
            column,
        };
        let Some(do_sql) = sql.get(g.start..g.end) else {
            residue_do_indices.push(i); // unreadable source range — conservative residue, never silent
            continue;
        };
        let analysis = plpgsql::analyze_do_block(do_sql);
        let mut block_residue = analysis.has_residue;
        for embedded in &analysis.statements {
            let Ok(embedded_parsed) = crate::ast::parse(embedded) else {
                block_residue = true; // recovered text we cannot re-parse — never silently drop a hazard
                continue;
            };
            for inner in &embedded_parsed.protobuf.stmts {
                let Some(inner_node) = inner.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
                    block_residue = true; // parsed but unreadable node — never silently skip a recovered statement
                    continue;
                };
                for rule in rules {
                    if options.disabled_rules.contains(rule.id()) {
                        continue;
                    }
                    rule.check(inner_node, &mut hits);
                    for h in hits.drain(..) {
                        findings.push(Finding {
                            rule_id: rule.id().to_string(),
                            severity: rule.severity(),
                            message: format!("Inside a DO block: {}", h.message),
                            guidance: h.guidance,
                            statement_index: i,
                            location,
                            snippet: embedded.trim().to_string(),
                            suppression: None,
                            fix: None,
                        });
                    }
                }
            }
        }
        if block_residue {
            residue_do_indices.push(i);
        }
    }
    if !options.disabled_rules.contains(timeout::ID) {
        let timeout_indices =
            timeout::require_timeout_indices(stmts, options.assume_in_transaction);
        // Build the fix draft once from the first flagged statement. The prologue is a single
        // migration-level edit — only the first finding carries it; the rest get None so that a
        // consumer splicing every finding's fix doesn't insert the prologue N times.
        let timeout_fix = timeout_indices.first().and_then(|&first| {
            // Anchor at `prologue_anchor` — the start of the statement's contiguous
            // own-line leading comment block (or the statement's own line start when
            // there is none) — so the prologue lands ABOVE any `-- pgsafe:ignore`
            // directives, not between a directive and the statement body.
            let start = u32::try_from(geoms[first].prologue_anchor).ok()?;
            Some(timeout::timeout_fix(start))
        });
        // `take()` yields Some for the first iteration, None for every subsequent one.
        let mut timeout_fix_once = timeout_fix;
        push_synthesized(
            &mut findings,
            sql,
            geoms,
            timeout::ID,
            Severity::Warning,
            timeout_indices.into_iter().map(|i| {
                let fix = timeout_fix_once.take();
                (
                    i,
                    timeout::MESSAGE.to_string(),
                    timeout::GUIDANCE.to_string(),
                    fix,
                )
            }),
        );
    }
    let (mut findings, new_table_dropped) = newtable::drop_new_table_findings(stmts, findings);
    // Per-hit severity: ATTACH PARTITION of a pre-existing child (not created/CHECK-prepared in
    // this migration) is error-grade. Runs before `severity_overrides` so explicit config wins.
    newtable::escalate_pre_existing_attach(stmts, &mut findings);
    if !options.disabled_rules.contains(txn::ID) {
        push_synthesized(
            &mut findings,
            sql,
            geoms,
            txn::ID,
            Severity::Error,
            txn::concurrently_in_transaction_indices(stmts, options.assume_in_transaction)
                .into_iter()
                .map(|i| (i, txn::MESSAGE.to_string(), txn::GUIDANCE.to_string(), None)),
        );
    }
    if !options.disabled_rules.contains(identifier::ID) {
        push_synthesized(
            &mut findings,
            sql,
            geoms,
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
                    None,
                ))
            }),
        );
    }
    if !options.disabled_rules.contains(fk_index::ID) {
        push_synthesized(
            &mut findings,
            sql,
            geoms,
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
                    None,
                )
            }),
        );
    }
    if !options.disabled_rules.contains(enum_value::ID) {
        push_synthesized(
            &mut findings,
            sql,
            geoms,
            enum_value::ID,
            Severity::Warning,
            enum_value::unsafe_enum_value_indices(sql, stmts, options.assume_in_transaction)
                .into_iter()
                .map(|i| {
                    (
                        i,
                        enum_value::MESSAGE.to_string(),
                        enum_value::GUIDANCE.to_string(),
                        None,
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
            geoms,
            require_pk::ID,
            Severity::Warning,
            require_pk::tables_without_primary_key(stmts)
                .into_iter()
                .map(|i| {
                    (
                        i,
                        require_pk::MESSAGE.to_string(),
                        require_pk::GUIDANCE.to_string(),
                        None,
                    )
                }),
        );
    }
    if options.enabled_rules.contains(require_schema_qualified::ID)
        && !options
            .disabled_rules
            .contains(require_schema_qualified::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            geoms,
            require_schema_qualified::ID,
            Severity::Warning,
            require_schema_qualified::unqualified_targets(stmts)
                .into_iter()
                .map(|(i, name)| {
                    (
                        i,
                        format!(
                            "Unqualified table name `{name}` resolves through search_path, which is \
                             environment-dependent — a migration footgun."
                        ),
                        require_schema_qualified::GUIDANCE.to_string(),
                        None,
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
            geoms,
            require_not_null::ID,
            Severity::Warning,
            require_not_null::nullable_columns(stmts)
                .into_iter()
                .map(|(i, message)| (i, message, require_not_null::GUIDANCE.to_string(), None)),
        );
    }
    if !options.naming_patterns.is_empty() && !options.disabled_rules.contains(naming::ID) {
        push_synthesized(
            &mut findings,
            sql,
            geoms,
            naming::ID,
            Severity::Warning,
            naming::naming_violations(stmts, &options.naming_patterns)
                .into_iter()
                .map(|(i, message)| (i, message, naming::GUIDANCE.to_string(), None)),
        );
    }
    if !options.forbidden_column_types.is_empty()
        && !options.disabled_rules.contains(forbidden_types::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            geoms,
            forbidden_types::ID,
            Severity::Warning,
            forbidden_types::forbidden_violations(stmts, &options.forbidden_column_types, sql)
                .into_iter()
                .map(|(i, message, fix)| (i, message, forbidden_types::GUIDANCE.to_string(), fix)),
        );
    }
    if options.enabled_rules.contains(require_if_exists::ID)
        && !options.disabled_rules.contains(require_if_exists::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            geoms,
            require_if_exists::ID,
            Severity::Warning,
            require_if_exists::missing_if_exists(stmts)
                .into_iter()
                .map(|(i, message, draft)| {
                    (i, message, require_if_exists::GUIDANCE.to_string(), draft)
                }),
        );
    }
    if options.enabled_rules.contains(do_block::ID)
        && !options.disabled_rules.contains(do_block::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            geoms,
            do_block::ID,
            Severity::Warning,
            residue_do_indices.iter().map(|&i| {
                (
                    i,
                    do_block::MESSAGE.to_string(),
                    do_block::GUIDANCE.to_string(),
                    None,
                )
            }),
        );
    }
    if options.enabled_rules.contains(require_comment::ID)
        && !options.disabled_rules.contains(require_comment::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            geoms,
            require_comment::ID,
            Severity::Warning,
            require_comment::missing_comments(stmts)
                .into_iter()
                .map(|(i, message)| (i, message, require_comment::GUIDANCE.to_string(), None)),
        );
    }
    if !options.required_columns.is_empty() && !options.disabled_rules.contains(require_columns::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            geoms,
            require_columns::ID,
            Severity::Warning,
            require_columns::missing_required_columns(stmts, &options.required_columns)
                .into_iter()
                .map(|(i, message)| (i, message, require_columns::GUIDANCE.to_string(), None)),
        );
    }
    if options.enabled_rules.contains(forbid_nullable_fk::ID)
        && !options.disabled_rules.contains(forbid_nullable_fk::ID)
    {
        push_synthesized(
            &mut findings,
            sql,
            geoms,
            forbid_nullable_fk::ID,
            Severity::Warning,
            forbid_nullable_fk::nullable_fk_columns(stmts)
                .into_iter()
                .map(|(i, message)| (i, message, forbid_nullable_fk::GUIDANCE.to_string(), None)),
        );
    }
    (findings, new_table_dropped)
}
