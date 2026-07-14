//! Auto-fix construction: a rule emits a [`FixDraft`] that expresses fix INTENT
//! as one or more anchored edits (absolute offsets, keyword positions, or
//! statement-relative anchors); the engine lowers each anchor to a validated
//! absolute byte [`crate::FixEdit`] using the source text and the statement's byte
//! span. A draft whose intent can't be located in the source resolves to `None`,
//! so an un-locatable fix is simply omitted rather than misapplied.

use crate::{Fix, FixEdit};

#[cfg(any(feature = "cli", feature = "lsp"))]
use crate::{FileReport, Finding, Severity};
#[cfg(any(feature = "cli", feature = "lsp"))]
use std::collections::HashMap;

/// Where an edit attaches, in terms a rule can express without the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FixAnchor {
    /// Absolute byte span the rule computed itself (e.g. from a node `location`).
    Absolute { start: u32, end: u32 },
    /// Insert at the statement's first-token byte (`span.start`).
    #[allow(dead_code)] // Plan 2 producer: reserved for statement-prologue insertions
    StatementStart,
    /// Insert at the statement body's end (`geoms[i].body_end` — after the last real token, before
    /// any trailing comment or `;`).
    StatementBodyEnd,
    /// Insert immediately after the first whole-word, ASCII-case-insensitive
    /// occurrence of this keyword within the statement span.
    AfterKeyword(&'static str),
    /// Replace the identifier token starting at this absolute byte offset.
    ReplaceTokenAt(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FixDraftEdit {
    pub anchor: FixAnchor,
    pub replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FixDraft {
    pub title: &'static str,
    pub edits: Vec<FixDraftEdit>,
}

impl FixDraft {
    /// Whether this draft may legitimately resolve to `None` rather than that
    /// indicating a producer bug. `ReplaceTokenAt` can fail for a quoted or
    /// schema-qualified type token; keyword/statement/absolute anchors cannot.
    ///
    /// Note: this whitelists ANY draft that contains at least one `ReplaceTokenAt` edit —
    /// including drafts from producers that pre-screen at the producer level (e.g.
    /// forbidden-column-type's `is_single_token_type`), so they also get the relaxation;
    /// production behaviour is unchanged because those producers already suppress the draft.
    pub(crate) fn may_legitimately_not_resolve(&self) -> bool {
        self.edits
            .iter()
            .any(|e| matches!(e.anchor, FixAnchor::ReplaceTokenAt(_)))
    }
}

/// Resolve a draft against the source. `start` is the statement's first-token offset
/// (`geoms[i].start`); `body_end` is one past its last real token, before any trailing comment
/// (`geoms[i].body_end`) — the correct right-hand bound for a body-end insertion and for the
/// keyword-search window, so a fix never lands in (or matches a keyword inside) a trailing comment.
/// Returns `None` if any anchor can't be located, or if the draft carries no edits (upholding
/// "fix present ⇒ at least one edit").
pub(crate) fn resolve(draft: &FixDraft, sql: &str, start: usize, body_end: usize) -> Option<Fix> {
    if draft.edits.is_empty() {
        return None;
    }
    let mut edits = Vec::with_capacity(draft.edits.len());
    for e in &draft.edits {
        let (s, en) = match e.anchor {
            FixAnchor::Absolute { start, end } => {
                let (s, e) = (start as usize, end as usize);
                // bounds-guard: out-of-range offset → None; point insertion (s==e) is Some("")
                sql.get(s..e)?;
                (s, e)
            }
            FixAnchor::StatementStart => (start, start),
            FixAnchor::StatementBodyEnd => (body_end, body_end),
            FixAnchor::AfterKeyword(kw) => {
                let at = keyword_end(sql.get(start..body_end)?, kw)? + start;
                (at, at)
            }
            FixAnchor::ReplaceTokenAt(at) => {
                let at = at as usize;
                let tok = token_len(sql.get(at..)?)?;
                // If the token is immediately followed (after optional whitespace) by `.`, it is
                // a schema qualifier (e.g. `pg_catalog.json`). Replacing it would produce corrupt
                // SQL (e.g. `jsonb.json`), so suppress the fix by returning None.
                if sql
                    .get(at + tok..)
                    .is_some_and(|s| s.trim_start().starts_with('.'))
                {
                    return None;
                }
                (at, at + tok)
            }
        };
        edits.push(FixEdit {
            start: u32::try_from(s).ok()?,
            end: u32::try_from(en).ok()?,
            replacement: e.replacement.clone(),
        });
    }
    // Uphold the Fix.edits invariant: ascending start order, non-overlapping.
    edits.sort_by_key(|e| e.start);
    for w in edits.windows(2) {
        debug_assert!(
            w[0].end <= w[1].start,
            "resolve produced overlapping edits: prev.end={} > next.start={}",
            w[0].end,
            w[1].start
        );
    }
    Some(Fix {
        title: draft.title.to_string(),
        edits,
    })
}

/// Byte offset one past the first whole-word, case-insensitive match of `kw` in
/// `hay`. Whole-word = not flanked by ASCII alphanumerics or `_`.
fn keyword_end(hay: &str, kw: &str) -> Option<usize> {
    let (hl, kl) = (hay.as_bytes(), kw.as_bytes());
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i + kl.len() <= hl.len() {
        if hl[i..i + kl.len()].eq_ignore_ascii_case(kl)
            && (i == 0 || !is_word(hl[i - 1]))
            && (i + kl.len() == hl.len() || !is_word(hl[i + kl.len()]))
        {
            return Some(i + kl.len());
        }
        i += 1;
    }
    None
}

/// Byte length of the identifier token at the start of `s` (ASCII alphanumerics
/// and `_`). `None` if `s` doesn't start with one.
fn token_len(s: &str) -> Option<usize> {
    let n = s
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    (n > 0).then_some(n)
}

/// Apply `edits` to `sql`, returning the rewritten string. Edits are applied high
/// offset to low so earlier splices don't shift later ones.
pub(crate) fn apply(sql: &str, edits: &[FixEdit]) -> String {
    let mut out = sql.to_string();
    let mut edits = edits.to_vec();
    edits.sort_by_key(|e| std::cmp::Reverse(e.start));
    for e in edits {
        out.replace_range(e.start as usize..e.end as usize, &e.replacement);
    }
    out
}

/// Outcome of composing a set of fixes onto one input.
pub(crate) struct Applied {
    /// The rewritten SQL after all accepted fixes were spliced.
    pub sql: String,
    /// The accepted edits, ascending by `start`, non-overlapping.
    pub edits: Vec<FixEdit>,
    /// Number of fixes whose edits were applied.
    pub applied: usize,
    /// Number of resolvable fixes skipped because an edit overlapped an already-accepted span.
    pub skipped_overlapping: usize,
}

/// Compose `fixes` (considered in slice order) onto `sql`. A fix is accepted only
/// if none of its edits overlaps a span already claimed by an accepted fix — a fix
/// is atomic (all of its edits apply, or the whole fix is skipped). The accepted
/// edits are spliced by [`apply`] (which orders the splices high-to-low internally);
/// an empty accepted set splices nothing.
pub(crate) fn apply_all(sql: &str, fixes: &[&Fix]) -> Applied {
    let mut accepted: Vec<FixEdit> = Vec::new();
    let mut applied = 0usize;
    let mut skipped_overlapping = 0usize;
    for fix in fixes {
        // Half-open overlap: [a,b) and [c,d) overlap iff a < d && c < b.
        let overlaps = fix
            .edits
            .iter()
            .any(|e| accepted.iter().any(|a| e.start < a.end && a.start < e.end));
        if overlaps {
            skipped_overlapping += 1;
            continue;
        }
        accepted.extend(fix.edits.iter().cloned());
        applied += 1;
    }
    accepted.sort_by_key(|e| e.start);
    debug_assert!(
        accepted.windows(2).all(|w| w[0].end <= w[1].start),
        "apply_all produced overlapping merged edits"
    );
    Applied {
        sql: apply(sql, &accepted),
        edits: accepted,
        applied,
        skipped_overlapping,
    }
}

/// Count non-suppressed Error-severity findings by rule id. Keyed by rule id — not
/// statement index — so the lock_timeout prologue fix (which inserts a statement and
/// shifts every later index) never reads as a change.
#[cfg(any(feature = "cli", feature = "lsp"))]
fn error_counts(findings: &[Finding]) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    for f in findings {
        if !f.is_suppressed() && f.severity >= Severity::Error {
            *counts.entry(f.rule_id.as_str()).or_insert(0) += 1;
        }
    }
    counts
}

/// Whether the composed result `after` carries an Error the original `before` did not —
/// i.e. some fix introduced a new hazard (e.g. a runtime-invalid CONCURRENTLY that slipped
/// past draft-time withdrawal). Uses an Error floor so it holds independent of `--fail-on`.
///
/// This is a defense-in-depth backstop: draft-time withdrawal already removes the only known
/// regression (CONCURRENTLY-in-transaction), so today no real fix set reaches this check. The
/// invariant every fix must uphold: applying it never introduces a new Error the input lacked.
/// It intentionally does not police new *Warnings* or a same-rule Error swap (net Error count
/// per rule id unchanged) — keep that in mind when adding a fix whose output could differ.
#[cfg(any(feature = "cli", feature = "lsp"))]
fn introduces_new_error(before: &[Finding], after: &[Finding]) -> bool {
    let baseline = error_counts(before);
    error_counts(after)
        .into_iter()
        .any(|(rule, n)| n > baseline.get(rule).copied().unwrap_or(0))
}

/// Hard cap on fixpoint iterations. A deliberately generous bound: no real rule
/// cascades past a single applying pass today, so this ceiling is only ever reached
/// by a pathological or oscillating rule — the cap just lets such a rule stop
/// gracefully (last validated-good result kept) instead of looping forever.
#[cfg(any(feature = "cli", feature = "lsp"))]
pub(crate) const MAX_FIX_ITERATIONS: usize = 10;

/// A candidate the loop refused to accept, keeping the prior validated-good text.
#[cfg(any(feature = "cli", feature = "lsp"))]
pub(crate) enum Withheld {
    /// A candidate no longer parsed; its error message.
    ParseBroke(String),
    /// A candidate introduced a new Error vs the original.
    NewError,
}

/// How the fixpoint loop stopped. Exactly one of these holds per run, so the
/// withheld-note and non-convergence-note diagnostics are mutually exclusive by
/// construction (a contradictory pair is unrepresentable).
#[cfg(any(feature = "cli", feature = "lsp"))]
pub(crate) enum Termination {
    /// Reached a fixpoint: no remaining fix, or a pass made no textual progress.
    Converged,
    /// A candidate was backed off (see [`Withheld`]); prior good text is kept.
    Withheld(Withheld),
    /// Hit the iteration cap while still making textual progress.
    CapHit,
}

/// Result of driving `apply_all` to a fixpoint over one input.
#[cfg(any(feature = "cli", feature = "lsp"))]
pub(crate) struct Fixpoint {
    /// Final text: parse-valid and regression-free vs the original (may equal it)
    /// whenever `final_report.error` is `None`; otherwise (the input itself didn't
    /// parse) it equals the unparseable original and is unused.
    pub(crate) sql: String,
    /// `Some(edits-against-original)` iff **exactly one** applying pass occurred (the
    /// universal case today whenever any fix applies) — lets `--diff` reuse the
    /// byte-exact edit renderer. `None` when zero passes apply (nothing changed) or
    /// ≥2 do (offsets have shifted; use an original-vs-final diff).
    pub(crate) edits: Option<Vec<FixEdit>>,
    /// Total fixes applied across all passes.
    pub(crate) applied: usize,
    /// Applying passes performed.
    pub(crate) iterations: usize,
    /// How the loop stopped (fixpoint, back-off, or cap) — see [`Termination`].
    pub(crate) termination: Termination,
    /// The original input's findings (regression baseline; also the `--diff` gate basis).
    pub(crate) original_findings: Vec<Finding>,
    /// Re-lint of `sql` (drives the `--fix` exit gate and the unfixable count).
    pub(crate) final_report: FileReport,
    /// Non-suppressed findings in `final_report` with no automatic fix.
    pub(crate) unfixable: usize,
    /// Residual overlap-skips in the last applying pass.
    pub(crate) skipped_overlapping: usize,
}

/// Drive `apply_all` to a fixpoint. `lint` is injected so the loop is testable
/// without CLI wiring and can be exercised with a simulated cascade no real rule
/// produces yet. Every accepted intermediate is validated (parses, introduces no
/// new Error vs `original`), so `sql` is always safe to write.
#[cfg(any(feature = "cli", feature = "lsp"))]
pub(crate) fn fix_to_fixpoint(
    original: &str,
    lint: impl Fn(&str) -> FileReport,
    max_iters: usize,
) -> Fixpoint {
    let original_report = lint(original);
    let original_findings = original_report.findings.clone();

    // If the input itself doesn't parse, there's nothing to do; surface the error
    // via `final_report` (the caller reports it exactly as before).
    if original_report.error.is_some() {
        return Fixpoint {
            sql: original.to_string(),
            edits: None,
            applied: 0,
            iterations: 0,
            termination: Termination::Converged,
            original_findings,
            final_report: original_report,
            unfixable: 0,
            skipped_overlapping: 0,
        };
    }

    let mut current = original.to_string();
    let mut report = original_report; // always == lint(current)
    let mut first_edits: Option<Vec<FixEdit>> = None;
    let mut iterations = 0usize;
    let mut applied_total = 0usize;
    let mut skipped_overlapping = 0usize;
    // Set on every natural stop; left `None` iff the loop exhausts `max_iters`.
    let mut termination: Option<Termination> = None;

    for _ in 0..max_iters {
        let fixes: Vec<&Fix> = report
            .findings
            .iter()
            .filter(|f| !f.is_suppressed())
            .filter_map(|f| f.fix.as_ref())
            .collect();
        if fixes.is_empty() {
            termination = Some(Termination::Converged); // nothing left to do
            break;
        }
        let step = crate::fix::apply_all(&current, &fixes);
        if step.sql == current {
            // No textual progress (remaining fixes all mutually overlap) — a fixpoint.
            skipped_overlapping = step.skipped_overlapping;
            termination = Some(Termination::Converged);
            break;
        }
        let candidate = lint(&step.sql);
        if let Some(err) = &candidate.error {
            termination = Some(Termination::Withheld(Withheld::ParseBroke(err.clone())));
            break;
        }
        if introduces_new_error(&original_findings, &candidate.findings) {
            termination = Some(Termination::Withheld(Withheld::NewError));
            break;
        }
        // Accept the candidate.
        current = step.sql;
        iterations += 1;
        applied_total += step.applied;
        skipped_overlapping = step.skipped_overlapping;
        first_edits = if iterations == 1 {
            Some(step.edits)
        } else {
            None
        };
        report = candidate; // == lint(current)
    }

    let unfixable = report
        .findings
        .iter()
        .filter(|f| !f.is_suppressed() && f.fix.is_none())
        .count();

    // No natural stop ⇒ the loop ran the full `max_iters`. Decide by the final
    // report: a non-suppressed finding still carrying a fix means we were cut off
    // mid-progress (CapHit); otherwise the last accepted pass actually reached the
    // fixpoint on the `max_iters`-th step, so it converged.
    let termination = termination.unwrap_or_else(|| {
        let still_fixable = report
            .findings
            .iter()
            .any(|f| !f.is_suppressed() && f.fix.is_some());
        if still_fixable {
            Termination::CapHit
        } else {
            Termination::Converged
        }
    });

    Fixpoint {
        sql: current,
        edits: first_edits,
        applied: applied_total,
        iterations,
        termination,
        original_findings,
        final_report: report,
        unfixable,
        skipped_overlapping,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(feature = "cli", feature = "lsp"))]
    use crate::{FileReport, Location, Severity};
    use crate::{Fix, FixEdit};

    // statement: "CREATE INDEX i ON t (c)" spanning bytes [0, 23)
    const SQL: &str = "CREATE INDEX i ON t (c);";

    #[test]
    fn after_keyword_inserts_past_the_word() {
        let d = FixDraft {
            title: "Add CONCURRENTLY",
            edits: vec![FixDraftEdit {
                anchor: FixAnchor::AfterKeyword("INDEX"),
                replacement: " CONCURRENTLY".into(),
            }],
        };
        let fix = resolve(&d, SQL, 0, 23).unwrap();
        // "CREATE INDEX" ends at byte 12.
        assert_eq!(
            fix.edits,
            vec![FixEdit {
                start: 12,
                end: 12,
                replacement: " CONCURRENTLY".into()
            }]
        );
        assert_eq!(
            apply(SQL, &fix.edits),
            "CREATE INDEX CONCURRENTLY i ON t (c);"
        );
    }

    #[test]
    fn after_keyword_is_case_insensitive_and_word_bounded() {
        let sql = "create index idx_index ON t (c);"; // 'index' also appears inside idx_index
        let d = FixDraft {
            title: "Add CONCURRENTLY",
            edits: vec![FixDraftEdit {
                anchor: FixAnchor::AfterKeyword("INDEX"),
                replacement: " CONCURRENTLY".into(),
            }],
        };
        let fix = resolve(&d, sql, 0, sql.len() - 1).unwrap();
        // Matches the keyword at bytes [7,12), not the substring inside idx_index.
        assert_eq!(
            apply(sql, &fix.edits),
            "create index CONCURRENTLY idx_index ON t (c);"
        );
    }

    #[test]
    fn after_keyword_absent_resolves_to_none() {
        let d = FixDraft {
            title: "x",
            edits: vec![FixDraftEdit {
                anchor: FixAnchor::AfterKeyword("MATERIALIZED"),
                replacement: " z".into(),
            }],
        };
        assert!(resolve(&d, SQL, 0, 23).is_none());
    }

    #[test]
    fn statement_body_end_inserts_before_semicolon() {
        // span end is 23 (before the ';'); body-end insert lands there.
        let d = FixDraft {
            title: "Add NOT VALID",
            edits: vec![FixDraftEdit {
                anchor: FixAnchor::StatementBodyEnd,
                replacement: " NOT VALID".into(),
            }],
        };
        let fix = resolve(&d, SQL, 0, 23).unwrap();
        assert_eq!(
            fix.edits,
            vec![FixEdit {
                start: 23,
                end: 23,
                replacement: " NOT VALID".into()
            }]
        );
        assert_eq!(apply(SQL, &fix.edits), "CREATE INDEX i ON t (c) NOT VALID;");
    }

    #[test]
    fn statement_start_inserts_a_prologue() {
        let d = FixDraft {
            title: "Set lock_timeout",
            edits: vec![FixDraftEdit {
                anchor: FixAnchor::StatementStart,
                replacement: "SET lock_timeout = '5s';\n".into(),
            }],
        };
        let fix = resolve(&d, SQL, 0, 23).unwrap();
        assert_eq!(
            apply(SQL, &fix.edits),
            "SET lock_timeout = '5s';\nCREATE INDEX i ON t (c);"
        );
    }

    #[test]
    fn replace_token_at_swaps_the_identifier() {
        let sql = "ALTER TABLE t ADD COLUMN c json;"; // 'json' starts at byte 27
        let d = FixDraft {
            title: "Use jsonb",
            edits: vec![FixDraftEdit {
                anchor: FixAnchor::ReplaceTokenAt(27),
                replacement: "jsonb".into(),
            }],
        };
        let fix = resolve(&d, sql, 0, sql.len() - 1).unwrap();
        assert_eq!(
            fix.edits,
            vec![FixEdit {
                start: 27,
                end: 31,
                replacement: "jsonb".into()
            }]
        );
        assert_eq!(apply(sql, &fix.edits), "ALTER TABLE t ADD COLUMN c jsonb;");
    }

    #[test]
    fn apply_handles_multiple_edits_high_to_low() {
        let fix = Fix {
            title: "t".into(),
            edits: vec![
                FixEdit {
                    start: 0,
                    end: 0,
                    replacement: "A".into(),
                },
                FixEdit {
                    start: 3,
                    end: 3,
                    replacement: "B".into(),
                },
            ],
        };
        assert_eq!(apply("xyz", &fix.edits), "AxyzB");
    }

    #[test]
    fn after_keyword_offsets_are_byte_correct_past_multibyte() {
        // a multi-byte char (é, 2 bytes) before the keyword must not desync offsets.
        let sql = "-- é\nCREATE INDEX i ON t (c);";
        let stmt_start = sql.find("CREATE").unwrap();
        let d = FixDraft {
            title: "Add CONCURRENTLY",
            edits: vec![FixDraftEdit {
                anchor: FixAnchor::AfterKeyword("INDEX"),
                replacement: " CONCURRENTLY".into(),
            }],
        };
        let fix = resolve(&d, sql, stmt_start, sql.len() - 1).unwrap();
        assert_eq!(
            apply(sql, &fix.edits),
            "-- é\nCREATE INDEX CONCURRENTLY i ON t (c);"
        );
    }

    #[test]
    fn replace_token_drafts_may_not_resolve() {
        let d = FixDraft {
            title: "t",
            edits: vec![FixDraftEdit {
                anchor: FixAnchor::ReplaceTokenAt(0),
                replacement: "x".into(),
            }],
        };
        assert!(d.may_legitimately_not_resolve());
        let k = FixDraft {
            title: "t",
            edits: vec![FixDraftEdit {
                anchor: FixAnchor::AfterKeyword("INDEX"),
                replacement: " y".into(),
            }],
        };
        assert!(!k.may_legitimately_not_resolve());
    }

    #[test]
    fn replace_token_at_suppresses_schema_qualifier() {
        // `pg_catalog.json` — `pg_catalog` is at byte 0, immediately followed by `.`.
        // Replacing it would produce `jsonb.json` (corrupt), so the engine must return None.
        let sql = "pg_catalog.json";
        let d = FixDraft {
            title: "Use jsonb",
            edits: vec![FixDraftEdit {
                anchor: FixAnchor::ReplaceTokenAt(0),
                replacement: "jsonb".into(),
            }],
        };
        assert!(resolve(&d, sql, 0, sql.len()).is_none());
    }

    #[test]
    fn resolve_rejects_empty_draft() {
        let d = FixDraft {
            title: "nothing",
            edits: vec![],
        };
        assert!(resolve(&d, SQL, 0, 23).is_none());
    }

    #[test]
    fn absolute_out_of_range_resolves_to_none() {
        // SQL is 24 bytes; byte 999 is far out of range.
        let d = FixDraft {
            title: "x",
            edits: vec![FixDraftEdit {
                anchor: FixAnchor::Absolute {
                    start: 999,
                    end: 1000,
                },
                replacement: "z".into(),
            }],
        };
        assert!(resolve(&d, SQL, 0, 23).is_none());
    }

    #[test]
    fn multi_edit_descending_apply_order_preserves_second_span() {
        // Two non-zero-width replacements where low-to-high application would corrupt the second:
        // "foo" [12,15) → "renamed_table" expands by 10 bytes, shifting "bigint"'s offset.
        // Descending application avoids the shift.
        let sql = "ALTER TABLE foo ADD COLUMN bar bigint;";
        let fix = Fix {
            title: "t".into(),
            edits: vec![
                FixEdit {
                    start: 12,
                    end: 15,
                    replacement: "renamed_table".into(),
                },
                FixEdit {
                    start: 31,
                    end: 37,
                    replacement: "int4".into(),
                },
            ],
        };
        assert_eq!(
            apply(sql, &fix.edits),
            "ALTER TABLE renamed_table ADD COLUMN bar int4;"
        );
    }

    #[test]
    fn apply_all_composes_two_nonoverlapping_fixes() {
        // "ALTER TABLE t ADD COLUMN c json;" — two independent edits:
        //   insert " IF NOT EXISTS" is not applicable here; use two real spans.
        let sql = "ALTER TABLE t ADD COLUMN c json;";
        let f1 = Fix {
            title: "Use jsonb".into(),
            edits: vec![FixEdit {
                start: 27,
                end: 31,
                replacement: "jsonb".into(),
            }],
        };
        // second fix: replace table name "t" [12,13) with "tbl"
        let f2 = Fix {
            title: "rename".into(),
            edits: vec![FixEdit {
                start: 12,
                end: 13,
                replacement: "tbl".into(),
            }],
        };
        let out = apply_all(sql, &[&f1, &f2]);
        assert_eq!(out.sql, "ALTER TABLE tbl ADD COLUMN c jsonb;");
        assert_eq!(out.applied, 2);
        assert_eq!(out.skipped_overlapping, 0);
        // accepted edits are returned ascending by start.
        assert_eq!(
            out.edits.iter().map(|e| e.start).collect::<Vec<_>>(),
            vec![12, 27]
        );
    }

    #[test]
    fn apply_all_skips_a_fix_overlapping_an_accepted_edit() {
        let sql = "ALTER TABLE t ADD COLUMN c json;";
        let first = Fix {
            title: "Use jsonb".into(),
            edits: vec![FixEdit {
                start: 27,
                end: 31,
                replacement: "jsonb".into(),
            }],
        };
        // overlaps [27,31): [29,31) — must be skipped, first wins.
        let clash = Fix {
            title: "clash".into(),
            edits: vec![FixEdit {
                start: 29,
                end: 31,
                replacement: "X".into(),
            }],
        };
        let out = apply_all(sql, &[&first, &clash]);
        assert_eq!(out.sql, "ALTER TABLE t ADD COLUMN c jsonb;");
        assert_eq!(out.applied, 1);
        assert_eq!(out.skipped_overlapping, 1);
    }

    #[test]
    fn apply_all_empty_is_unchanged() {
        let out = apply_all("SELECT 1;", &[]);
        assert_eq!(out.sql, "SELECT 1;");
        assert_eq!(out.applied, 0);
        assert_eq!(out.skipped_overlapping, 0);
        assert!(out.edits.is_empty());
    }

    // --- fixpoint driver test helpers ---

    #[cfg(any(feature = "cli", feature = "lsp"))]
    fn finding_with_fix(rule: &str, sev: Severity, fix: Option<Fix>) -> crate::Finding {
        crate::Finding {
            rule_id: rule.into(),
            severity: sev,
            message: String::new(),
            guidance: String::new(),
            statement_index: 0,
            location: Location {
                byte: 0,
                line: 1,
                column: 1,
            },
            snippet: String::new(),
            suppression: None,
            fix,
        }
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    fn report_with(findings: Vec<crate::Finding>) -> FileReport {
        FileReport {
            name: "t".into(),
            findings,
            error: None,
        }
    }

    // Replace the single char at byte 0 with `to`.
    #[cfg(any(feature = "cli", feature = "lsp"))]
    fn swap_first_char(to: &str) -> Fix {
        Fix {
            title: "swap".into(),
            edits: vec![FixEdit {
                start: 0,
                end: 1,
                replacement: to.into(),
            }],
        }
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    #[test]
    fn introduces_new_error_detects_a_new_error_rule() {
        let mk = |rule: &str, sev| crate::Finding {
            rule_id: rule.into(),
            severity: sev,
            message: String::new(),
            guidance: String::new(),
            statement_index: 0,
            location: Location {
                byte: 0,
                line: 1,
                column: 1,
            },
            snippet: String::new(),
            suppression: None,
            fix: None,
        };
        let before = vec![mk("add-index-non-concurrent", Severity::Error)];
        // A fix cleared add-index but introduced a new runtime-failure Error.
        let after = vec![mk("concurrently-in-transaction", Severity::Error)];
        assert!(super::introduces_new_error(&before, &after));
        // No new error: fewer/equal Errors is not a regression.
        assert!(!super::introduces_new_error(&before, &[]));
        assert!(!super::introduces_new_error(&before, &before));
        // A new Warning is not a regression (Error floor).
        assert!(!super::introduces_new_error(
            &before,
            &[mk("x", Severity::Warning)]
        ));
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    #[test]
    fn fixpoint_cascades_until_no_fix() {
        // "1"→"2"→"3"→none, Warning severity so the no-regression check never fires.
        let lint = |s: &str| match s {
            "1" => report_with(vec![finding_with_fix(
                "r",
                Severity::Warning,
                Some(swap_first_char("2")),
            )]),
            "2" => report_with(vec![finding_with_fix(
                "r",
                Severity::Warning,
                Some(swap_first_char("3")),
            )]),
            _ => report_with(vec![]),
        };
        let fp = super::fix_to_fixpoint("1", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "3");
        assert_eq!(fp.iterations, 2);
        assert!(matches!(fp.termination, super::Termination::Converged));
        assert_eq!(fp.applied, 2);
        // Two applying passes → original-coordinate edits are no longer valid.
        assert!(fp.edits.is_none());
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    #[test]
    fn fixpoint_single_pass_keeps_original_edits() {
        // One fix, converges after one apply → edits available for the byte-exact diff.
        let lint = |s: &str| match s {
            "1" => report_with(vec![finding_with_fix(
                "r",
                Severity::Warning,
                Some(swap_first_char("2")),
            )]),
            _ => report_with(vec![]),
        };
        let fp = super::fix_to_fixpoint("1", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "2");
        assert_eq!(fp.iterations, 1);
        assert!(matches!(fp.termination, super::Termination::Converged));
        assert_eq!(
            fp.edits.as_deref(),
            Some(
                &[FixEdit {
                    start: 0,
                    end: 1,
                    replacement: "2".into()
                }][..]
            )
        );
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    #[test]
    fn fixpoint_no_fixes_is_a_noop() {
        let lint = |_: &str| report_with(vec![]);
        let fp = super::fix_to_fixpoint("SELECT 1;", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "SELECT 1;");
        assert_eq!(fp.iterations, 0);
        assert!(matches!(fp.termination, super::Termination::Converged));
        assert!(fp.edits.is_none());
        assert_eq!(fp.applied, 0);
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    #[test]
    fn fixpoint_oscillation_hits_cap() {
        // "a"→"b"→"a"→… never stabilizes; the cap stops it with the last good text.
        let lint = |s: &str| match s {
            "a" => report_with(vec![finding_with_fix(
                "r",
                Severity::Warning,
                Some(swap_first_char("b")),
            )]),
            "b" => report_with(vec![finding_with_fix(
                "r",
                Severity::Warning,
                Some(swap_first_char("a")),
            )]),
            _ => report_with(vec![]),
        };
        let fp = super::fix_to_fixpoint("a", lint, 4);
        assert!(matches!(fp.termination, super::Termination::CapHit));
        assert_eq!(fp.iterations, 4);
        // Result is one of the validated intermediates — always parse-valid SQL.
        assert!(fp.sql == "a" || fp.sql == "b");
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    #[test]
    fn fixpoint_backs_off_new_error_on_second_pass() {
        // Pass 1 (Warning) applies; pass 2's result introduces a NEW Error rule → back off.
        let lint = |s: &str| match s {
            "1" => report_with(vec![finding_with_fix(
                "r",
                Severity::Warning,
                Some(swap_first_char("2")),
            )]),
            "2" => report_with(vec![
                finding_with_fix("r", Severity::Warning, Some(swap_first_char("3"))),
                // no error yet at "2"; the error appears only in the *candidate* "3":
            ]),
            "3" => report_with(vec![finding_with_fix("boom", Severity::Error, None)]),
            _ => report_with(vec![]),
        };
        let fp = super::fix_to_fixpoint("1", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "2"); // kept the last validated-good result
        assert_eq!(fp.iterations, 1);
        assert!(matches!(
            fp.termination,
            super::Termination::Withheld(super::Withheld::NewError)
        ));
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    #[test]
    fn fixpoint_backs_off_on_first_pass_leaves_original() {
        // The very first candidate introduces a new Error → nothing accepted, original kept.
        let lint = |s: &str| match s {
            "1" => report_with(vec![finding_with_fix(
                "r",
                Severity::Warning,
                Some(swap_first_char("2")),
            )]),
            _ => report_with(vec![finding_with_fix("boom", Severity::Error, None)]),
        };
        let fp = super::fix_to_fixpoint("1", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "1");
        assert_eq!(fp.iterations, 0);
        assert!(matches!(
            fp.termination,
            super::Termination::Withheld(super::Withheld::NewError)
        ));
        assert!(fp.edits.is_none());
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    #[test]
    fn fixpoint_backs_off_parse_break() {
        let lint = |s: &str| match s {
            "ok" => report_with(vec![finding_with_fix(
                "r",
                Severity::Warning,
                Some(swap_first_char("X")),
            )]),
            _ => FileReport {
                name: "t".into(),
                findings: vec![],
                error: Some("boom".into()),
            },
        };
        let fp = super::fix_to_fixpoint("ok", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "ok");
        assert!(matches!(
            fp.termination,
            super::Termination::Withheld(super::Withheld::ParseBroke(_))
        ));
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    #[test]
    fn fixpoint_cap_reached_exactly_at_convergence() {
        // A cascade that produces exactly N fixes then none, run with max_iters = N:
        // the final accepted pass lands the fixpoint on the Nth step. The loop
        // exhausts the cap without a natural break, but the final report carries no
        // remaining fix, so this is Converged — not the false CapHit the old
        // `iterations == max_iters` shortcut would have reported.
        let lint = |s: &str| match s {
            "1" => report_with(vec![finding_with_fix(
                "r",
                Severity::Warning,
                Some(swap_first_char("2")),
            )]),
            "2" => report_with(vec![finding_with_fix(
                "r",
                Severity::Warning,
                Some(swap_first_char("3")),
            )]),
            "3" => report_with(vec![finding_with_fix(
                "r",
                Severity::Warning,
                Some(swap_first_char("4")),
            )]),
            _ => report_with(vec![]),
        };
        let fp = super::fix_to_fixpoint("1", lint, 3);
        assert!(
            matches!(fp.termination, super::Termination::Converged),
            "reached the fixpoint exactly at the cap"
        );
        assert_eq!(fp.sql, "4");
        assert_eq!(fp.iterations, 3);
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    #[test]
    fn fixpoint_recovers_skipped_overlap_on_next_pass() {
        // Pass 1 emits two fixes whose edits overlap at byte 0: `apply_all` accepts
        // the first ([0,1)→"X") and skips the second for overlap. Pass 2's re-lint
        // re-reports the loser with a now-non-overlapping fix ([1,2)→"Y") that lands.
        // The driver must recover it across passes and end clean with no residual skip.
        let lint = |s: &str| match s {
            "ab" => report_with(vec![
                finding_with_fix("winner", Severity::Warning, Some(swap_first_char("X"))),
                finding_with_fix(
                    "loser",
                    Severity::Warning,
                    Some(Fix {
                        title: "loser".into(),
                        edits: vec![FixEdit {
                            start: 0,
                            end: 2,
                            replacement: "?".into(),
                        }],
                    }),
                ),
            ]),
            "Xb" => report_with(vec![finding_with_fix(
                "loser",
                Severity::Warning,
                Some(Fix {
                    title: "loser".into(),
                    edits: vec![FixEdit {
                        start: 1,
                        end: 2,
                        replacement: "Y".into(),
                    }],
                }),
            )]),
            _ => report_with(vec![]),
        };
        let fp = super::fix_to_fixpoint("ab", lint, super::MAX_FIX_ITERATIONS);
        assert!(matches!(fp.termination, super::Termination::Converged));
        assert_eq!(fp.sql, "XY"); // the skipped fix landed on the second pass
        assert_eq!(fp.applied, 2);
        assert_eq!(fp.skipped_overlapping, 0); // no residual skip after recovery
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    #[test]
    fn fixpoint_no_textual_progress_converges() {
        // A fix whose edit is a textual no-op (replace byte 0 with itself) makes a
        // pass produce no change; the loop treats that as a fixpoint, not a cap hit.
        let lint = |s: &str| match s {
            "x" => report_with(vec![finding_with_fix(
                "noop",
                Severity::Warning,
                Some(swap_first_char("x")),
            )]),
            _ => report_with(vec![]),
        };
        let fp = super::fix_to_fixpoint("x", lint, super::MAX_FIX_ITERATIONS);
        assert!(matches!(fp.termination, super::Termination::Converged));
        assert_eq!(fp.iterations, 0);
        assert_eq!(fp.sql, "x");
    }

    #[cfg(any(feature = "cli", feature = "lsp"))]
    #[test]
    fn real_inputs_converge_in_one_pass() {
        use crate::LintOptions;
        let opts = LintOptions::default();
        // Representative single- and multi-fix inputs across the fix-producing rules.
        let inputs = [
            "CREATE INDEX i ON t (c);",
            "ALTER TABLE t ADD COLUMN c json;",
            "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id);",
            "REINDEX INDEX i;",
            "DROP INDEX i;",
            // A statement that triggers two independent fixes at once:
            "CREATE INDEX i ON t (c);\nALTER TABLE t ADD COLUMN c json;",
        ];
        for sql in inputs {
            let lint = |s: &str| crate::lint_input("<test>", s, &opts);
            let fp = super::fix_to_fixpoint(sql, lint, super::MAX_FIX_ITERATIONS);
            assert!(
                fp.iterations <= 1,
                "input unexpectedly cascaded ({} passes): {sql:?}",
                fp.iterations
            );
            assert!(
                matches!(fp.termination, super::Termination::Converged),
                "input did not converge: {sql:?}"
            );
            // When a fix applied, the fixpoint result matches a single apply_all.
            if fp.iterations == 1 {
                let report = crate::lint_input("<test>", sql, &opts);
                let fixes: Vec<&crate::Fix> = report
                    .findings
                    .iter()
                    .filter(|f| !f.is_suppressed())
                    .filter_map(|f| f.fix.as_ref())
                    .collect();
                let once = crate::fix::apply_all(sql, &fixes);
                assert_eq!(
                    fp.sql, once.sql,
                    "fixpoint diverged from single-pass for {sql:?}"
                );
            }
        }
    }
}
