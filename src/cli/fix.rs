//! Fix-mode CLI: apply fixes (`--fix`) or preview them as a unified diff (`--diff`).

use std::collections::HashMap;
use std::process::ExitCode;

use super::ResolvedRun;
use crate::{gate, lint_input, FileReport, Finding, Fix, FixEdit, Severity};

/// Stdin inputs carry this display-name (see `cli::mod::read_inputs`).
const STDIN_NAME: &str = "<stdin>";

/// Which fix-mode operation to run.
pub(super) enum Mode {
    /// Apply fixes: rewrite files in place; stdin → fixed SQL on stdout.
    Apply,
    /// Preview fixes as a unified diff; write nothing.
    Diff,
}

/// Count non-suppressed Error-severity findings by rule id. Keyed by rule id — not
/// statement index — so the lock_timeout prologue fix (which inserts a statement and
/// shifts every later index) never reads as a change.
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
fn introduces_new_error(before: &[Finding], after: &[Finding]) -> bool {
    let baseline = error_counts(before);
    error_counts(after)
        .into_iter()
        .any(|(rule, n)| n > baseline.get(rule).copied().unwrap_or(0))
}

/// Run fix mode over the resolved inputs. Summaries go to stderr; `--fix` on
/// stdin and all `--diff` output go to stdout. `--fix` re-lints the composed
/// text before writing and refuses to write if the fixes broke parsing. Exit: 2
/// on any parse/IO error; otherwise `--fix` gates on that pre-write re-lint,
/// `--diff` on the original findings → 1 if gated findings remain, else 0. When
/// gating findings survive that no fix touched, a stderr note explains why (never
/// a silent nonzero exit).
pub(super) fn run(r: &ResolvedRun, mode: Mode) -> ExitCode {
    let mut had_error = false;
    let mut gated = false;

    for (name, sql) in &r.inputs {
        let report = lint_input(name.clone(), sql, &r.options_for(name));
        if let Some(err) = &report.error {
            eprintln!("error: {name}: {err}");
            had_error = true;
            continue;
        }
        // Fixes from non-suppressed findings only; count non-suppressed unfixable.
        let fixes: Vec<&Fix> = report
            .findings
            .iter()
            .filter(|f| !f.is_suppressed())
            .filter_map(|f| f.fix.as_ref())
            .collect();
        let unfixable = report
            .findings
            .iter()
            .filter(|f| !f.is_suppressed() && f.fix.is_none())
            .count();

        let applied = crate::fix::apply_all(sql, &fixes);

        match mode {
            Mode::Diff => {
                // Re-lint the composed result. If applying the fixes would break parsing or
                // introduce a new Error, show no diff for this file — matching what `--fix`
                // refuses to write, so a `--diff` preview never differs from what `--fix` does.
                let after = lint_input(name.clone(), &applied.sql, &r.options_for(name));
                if let Some(err) = &after.error {
                    eprintln!(
                        "error: {name}: applying fixes produced SQL that no longer parses ({err}); no diff shown"
                    );
                    had_error = true;
                    continue;
                }
                if introduces_new_error(&report.findings, &after.findings) {
                    eprintln!(
                        "{name}: fixes withheld — applying them would introduce a new issue; lint `{name}` to see the findings"
                    );
                    if gate(&report.findings, r.fail_on) {
                        gated = true;
                    }
                    continue;
                }
                print!("{}", render_diff(name, sql, &applied.edits));
                if gate(&report.findings, r.fail_on) {
                    gated = true;
                    // Residual gating findings the diff can't show: those with no
                    // automatic fix, PLUS fixable ones whose edit `apply_all` skipped
                    // for overlapping an accepted fix. Both are absent from the diff, so
                    // report them rather than exiting nonzero in silence — even when the
                    // diff above is non-empty (a mixed fixable/unfixable file).
                    let residual = unfixable + applied.skipped_overlapping;
                    if residual > 0 {
                        eprintln!(
                            "{name}: {residual} finding(s) have no automatic fix or were skipped (overlapping another fix); lint `{name}` to see them"
                        );
                    }
                }
            }
            Mode::Apply => {
                // Verify the composed result still parses BEFORE writing anything;
                // this same re-lint also drives the exit gate below.
                let after = lint_input(name.clone(), &applied.sql, &r.options_for(name));
                if let Some(err) = &after.error {
                    eprintln!(
                        "error: {name}: applying fixes produced SQL that no longer parses ({err}); file left unchanged"
                    );
                    had_error = true;
                    continue;
                }
                // No-regression backstop: never write a result whose fixes introduced a new
                // Error the original lacked. Leave the file unchanged; the original findings
                // still drive the gate. (stdin still emits its unchanged input.)
                if introduces_new_error(&report.findings, &after.findings) {
                    if name == STDIN_NAME {
                        print!("{sql}");
                    }
                    eprintln!(
                        "{name}: fixes withheld — applying them would introduce a new issue; lint `{name}` to see the findings"
                    );
                    if gate(&report.findings, r.fail_on) {
                        gated = true;
                    }
                    continue;
                }
                let changed = applied.sql != *sql;
                if name == STDIN_NAME {
                    print!("{}", applied.sql);
                } else if changed {
                    if let Err(e) = write_atomic(name, &applied.sql) {
                        eprintln!("error: {name}: {e}");
                        had_error = true;
                        continue;
                    }
                }
                if changed {
                    let mut note = String::new();
                    if unfixable > 0 {
                        note.push_str(&format!("{unfixable} unfixable"));
                    }
                    if applied.skipped_overlapping > 0 {
                        if !note.is_empty() {
                            note.push_str(", ");
                        }
                        note.push_str(&format!(
                            "{} skipped-overlapping",
                            applied.skipped_overlapping
                        ));
                    }
                    let suffix = if note.is_empty() {
                        String::new()
                    } else {
                        format!(" ({note})")
                    };
                    eprintln!("fixed {} findings in {name}{suffix}", applied.applied);
                }
                // Exit gate: the pre-write re-lint of the composed text.
                if gate(&after.findings, r.fail_on) {
                    gated = true;
                    if !changed {
                        // Gating findings remain but nothing was rewritten: don't exit
                        // nonzero silently. (When `changed`, the "fixed …" line above
                        // already reports the unfixable/skipped count.)
                        let remaining =
                            after.findings.iter().filter(|f| !f.is_suppressed()).count();
                        eprintln!(
                            "{name}: {remaining} finding(s) remain that --fix cannot resolve; lint `{name}` to see them"
                        );
                    }
                }
            }
        }
    }

    if had_error {
        ExitCode::from(2)
    } else if gated {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Write `contents` to `path` atomically, as a faithful drop-in for an in-place
/// write: resolve symlinks so the **real** file is rewritten (not replaced by a
/// regular file), refuse a read-only target (preserving the "read-only file exits 2"
/// behavior), write a sibling temp of the resolved target and rename it over that
/// target (atomic within the directory; replaces on Unix and Windows), and preserve
/// the target's permission bits on the replacement inode (rename otherwise resets
/// them to the umask default). On error after the temp is created it attempts (does
/// not guarantee) to remove the temp, so normally no stray temp is left behind.
fn write_atomic(path: &str, contents: &str) -> std::io::Result<()> {
    use std::io::ErrorKind;
    // Resolve symlinks so we rewrite the REAL file (like the old in-place write)
    // instead of replacing the symlink with a regular file. Falls back to `path`
    // if it doesn't exist yet (canonicalize requires an existing path).
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| std::path::PathBuf::from(path));
    let meta = std::fs::metadata(&target).ok();
    if let Some(m) = &meta {
        if m.permissions().readonly() {
            return Err(std::io::Error::new(
                ErrorKind::PermissionDenied,
                "destination is read-only",
            ));
        }
    }
    // Sibling temp of the resolved target (same filesystem → atomic rename).
    let mut tmp = target.clone().into_os_string();
    tmp.push(format!(".pgsafe.{}.tmp", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    if let Err(e) = std::fs::write(&tmp, contents) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Preserve the target's permissions on the replacement inode (rename resets them).
    if let Some(m) = &meta {
        let _ = std::fs::set_permissions(&tmp, m.permissions());
    }
    if let Err(e) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// 0-based line index containing byte offset `off` (clamped to the last line).
/// `line_starts` are the byte offsets of each line's first character.
fn line_of(line_starts: &[usize], off: usize) -> usize {
    match line_starts.binary_search(&off) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    }
}

/// Unchanged context lines shown around each changed region. Also the basis for
/// hunk coalescing: two changed regions share one hunk when the unchanged gap
/// between them is at most `2 * CONTEXT` lines (their context would otherwise
/// meet or overlap), matching how `diff -u` merges nearby changes.
const CONTEXT: usize = 3;

/// Render `edits` against `original` as a `git apply`-able unified diff.
///
/// Edits (ascending, non-overlapping) are grouped into *blocks* by the original
/// lines they touch; each block shows those original lines (`-`) and the spliced
/// result (`+`). Blocks are surrounded by up to [`CONTEXT`] unchanged lines and
/// coalesced into shared hunks when close together. Headers use the git `a/` /
/// `b/` prefixes and a `\ No newline at end of file` marker is emitted whenever a
/// file's final shown line lacks a trailing newline, so the output round-trips
/// through `git apply`. Empty `edits` render nothing.
pub(super) fn render_diff(name: &str, original: &str, edits: &[FixEdit]) -> String {
    if edits.is_empty() {
        return String::new();
    }
    // Byte offset where each original line begins (line 0 at byte 0).
    let mut line_starts = vec![0usize];
    for (i, b) in original.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let orig_lines: Vec<&str> = original.split_inclusive('\n').collect();
    let total = orig_lines.len();

    // A maximal run of original lines touched by edits, plus the lines that
    // replace them (that region's original text with its edits spliced in).
    struct Block {
        first: usize,
        last: usize,
        new_lines: Vec<String>,
    }

    // Group edits whose touched original-line ranges are contiguous.
    struct Pending {
        first: usize,
        last: usize,
        edits: Vec<FixEdit>,
    }
    let mut pending: Vec<Pending> = Vec::new();
    for e in edits {
        let start_line = line_of(&line_starts, e.start as usize);
        // `end` is exclusive; the last touched line contains `end - 1` (or the
        // start line for a zero-width insertion).
        let end_byte = (e.end as usize).max(e.start as usize + 1) - 1;
        let end_line = line_of(&line_starts, end_byte);
        match pending.last_mut() {
            Some(p) if start_line <= p.last + 1 => {
                p.last = p.last.max(end_line);
                p.edits.push(e.clone());
            }
            _ => pending.push(Pending {
                first: start_line,
                last: end_line,
                edits: vec![e.clone()],
            }),
        }
    }

    // Splice each block's edits into its original text to get the replacement lines.
    let blocks: Vec<Block> = pending
        .into_iter()
        .map(|p| {
            let block_start = line_starts[p.first];
            let block_end = line_starts
                .get(p.last + 1)
                .copied()
                .unwrap_or(original.len());
            let mut new_block = original[block_start..block_end].to_string();
            let mut local: Vec<&FixEdit> = p.edits.iter().collect();
            local.sort_by_key(|e| e.start);
            for e in local.iter().rev() {
                let s = e.start as usize - block_start;
                let en = e.end as usize - block_start;
                new_block.replace_range(s..en, &e.replacement);
            }
            Block {
                first: p.first,
                last: p.last,
                new_lines: new_block
                    .split_inclusive('\n')
                    .map(str::to_string)
                    .collect(),
            }
        })
        .collect();

    // Coalesce blocks into hunks: a run of blocks each within `2 * CONTEXT`
    // unchanged lines of the previous shares one hunk (the gap shown as context).
    let mut hunks: Vec<Vec<usize>> = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        match hunks.last_mut() {
            Some(h) if b.first - blocks[h[h.len() - 1]].last - 1 <= 2 * CONTEXT => h.push(i),
            _ => hunks.push(vec![i]),
        }
    }

    let mut out = format!("--- a/{name}\n+++ b/{name}\n");
    // Running new-file line delta from all preceding hunks (context lines are
    // common to both files, so only removed/added lines shift it).
    let mut new_delta: isize = 0;
    for h in &hunks {
        let region_first = blocks[h[0]].first;
        let region_last = blocks[h[h.len() - 1]].last;
        // Every touched line index must be a real original line: today's fixes never
        // map an edit past the last line, but guard the `total - 1 - region_last`
        // subtraction and the `orig_lines[..=region_last]` slices against a future
        // edit that does (would otherwise underflow / index out of bounds).
        debug_assert!(
            region_last < total,
            "region_last {region_last} out of bounds for {total} original lines"
        );
        let ctx_before = region_first.min(CONTEXT);
        let ctx_after = (total - 1 - region_last).min(CONTEXT);
        let hunk_old_first = region_first - ctx_before;

        let mut body = String::new();
        let mut old_count = 0usize; // context + removed
        let mut new_count = 0usize; // context + added

        // Leading context.
        for &line in &orig_lines[hunk_old_first..region_first] {
            push_diff_line(&mut body, ' ', line);
            old_count += 1;
            new_count += 1;
        }
        // Each block, with unchanged gaps between blocks shown as context.
        for (j, &bi) in h.iter().enumerate() {
            let b = &blocks[bi];
            for &line in &orig_lines[b.first..=b.last] {
                push_diff_line(&mut body, '-', line);
                old_count += 1;
            }
            for nl in &b.new_lines {
                push_diff_line(&mut body, '+', nl);
                new_count += 1;
            }
            if let Some(&next_bi) = h.get(j + 1) {
                for &line in &orig_lines[(b.last + 1)..blocks[next_bi].first] {
                    push_diff_line(&mut body, ' ', line);
                    old_count += 1;
                    new_count += 1;
                }
            }
        }
        // Trailing context.
        for &line in &orig_lines[(region_last + 1)..(region_last + 1 + ctx_after)] {
            push_diff_line(&mut body, ' ', line);
            old_count += 1;
            new_count += 1;
        }

        let old_start = hunk_old_first + 1; // 1-based
                                            // Clamp to line 1: today's insert/replace fixes keep `new_delta >= 0`, but
                                            // clamp rather than panic if a future fix removes more lines than precede it.
        let new_start =
            usize::try_from(isize::try_from(old_start).unwrap_or(1) + new_delta).unwrap_or(1);
        out.push_str(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));
        out.push_str(&body);
        new_delta +=
            isize::try_from(new_count).unwrap_or(0) - isize::try_from(old_count).unwrap_or(0);
    }
    out
}

/// Push one unified-diff body line — `prefix` then `content` — appending git's
/// `\ No newline at end of file` marker when `content` (a file's final line)
/// carries no trailing newline. `split_inclusive('\n')` yields a marker-worthy
/// unterminated slice only for a file's/block's genuine last line, so this fires
/// exactly where git would.
///
/// Caveat: this assumes a fix never deletes a *mid-file* newline (merging two
/// lines). If one did, the block splice could leave a non-final line unterminated
/// and this would emit a spurious marker. None of today's fixes do so (they insert
/// or replace within a line — CONCURRENTLY, the timeout prologue, `json`→`jsonb`);
/// revisit here if a newline-removing fix is ever added.
fn push_diff_line(out: &mut String, prefix: char, content: &str) {
    out.push(prefix);
    out.push_str(content);
    if !content.ends_with('\n') {
        out.push('\n');
        out.push_str("\\ No newline at end of file\n");
    }
}

/// Hard cap on fixpoint iterations. Real cascades converge in 2–3 passes; a value
/// this high is only ever reached by a pathological oscillating rule, which the cap
/// stops gracefully (last validated-good result kept).
// Driver only: not yet wired into `run()` (task 3 of the fixpoint-iteration plan) — this
// task adds the driver + its unit tests, so nothing outside `#[cfg(test)]` uses it yet.
#[allow(dead_code)]
const MAX_FIX_ITERATIONS: usize = 10;

/// Why the loop stopped applying fixes before reaching a natural fixpoint.
// Driver only; see `MAX_FIX_ITERATIONS` above.
#[allow(dead_code)]
enum Withheld {
    /// A candidate no longer parsed; its error message.
    ParseBroke(String),
    /// A candidate introduced a new Error vs the original.
    NewError,
}

/// Result of driving `apply_all` to a fixpoint over one input.
// Driver only; see `MAX_FIX_ITERATIONS` above.
#[allow(dead_code)]
struct Fixpoint {
    /// Final text: parse-valid and regression-free vs the original (may equal it).
    sql: String,
    /// `Some(edits-against-original)` iff at most one applying pass occurred (the
    /// universal case today) — lets `--diff` reuse the byte-exact edit renderer.
    /// `None` once ≥2 passes apply (offsets have shifted; use an original-vs-final diff).
    edits: Option<Vec<FixEdit>>,
    /// Total fixes applied across all passes.
    applied: usize,
    /// Applying passes performed.
    iterations: usize,
    /// `false` iff the cap was hit while the text was still changing.
    converged: bool,
    /// The original input's findings (regression baseline; also the `--diff` gate basis).
    original_findings: Vec<Finding>,
    /// Re-lint of `sql` (drives the `--fix` exit gate and the unfixable count).
    final_report: FileReport,
    /// Non-suppressed findings in `final_report` with no automatic fix.
    unfixable: usize,
    /// Residual overlap-skips in the last applying pass.
    skipped_overlapping: usize,
    /// Set iff the loop backed off a candidate (parse-break or new Error).
    withheld: Option<Withheld>,
}

/// Drive `apply_all` to a fixpoint. `lint` is injected so the loop is testable
/// without CLI wiring and can be exercised with a simulated cascade no real rule
/// produces yet. Every accepted intermediate is validated (parses, introduces no
/// new Error vs `original`), so `sql` is always safe to write.
// Driver only; see `MAX_FIX_ITERATIONS` above. `name` is plumbed through for the CLI
// wiring (task 3) to use in its own diagnostics; the driver itself doesn't need it
// since every `lint` call already carries the name its caller bound the closure to.
#[allow(dead_code)]
fn fix_to_fixpoint(
    _name: &str,
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
            converged: true,
            original_findings,
            final_report: original_report,
            unfixable: 0,
            skipped_overlapping: 0,
            withheld: None,
        };
    }

    let mut current = original.to_string();
    let mut report = original_report; // always == lint(current)
    let mut first_edits: Option<Vec<FixEdit>> = None;
    let mut iterations = 0usize;
    let mut applied_total = 0usize;
    let mut skipped_overlapping = 0usize;
    let mut withheld: Option<Withheld> = None;
    let mut converged = false;

    for _ in 0..max_iters {
        let fixes: Vec<&Fix> = report
            .findings
            .iter()
            .filter(|f| !f.is_suppressed())
            .filter_map(|f| f.fix.as_ref())
            .collect();
        if fixes.is_empty() {
            converged = true; // nothing left to do
            break;
        }
        let step = crate::fix::apply_all(&current, &fixes);
        if step.sql == current {
            // No textual progress (remaining fixes all mutually overlap) — a fixpoint.
            skipped_overlapping = step.skipped_overlapping;
            converged = true;
            break;
        }
        let candidate = lint(&step.sql);
        if let Some(err) = &candidate.error {
            withheld = Some(Withheld::ParseBroke(err.clone()));
            converged = true; // clean stop with a valid `current`, not a cap overflow
            break;
        }
        if introduces_new_error(&original_findings, &candidate.findings) {
            withheld = Some(Withheld::NewError);
            converged = true;
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
    // The loop exhausted `max_iters` without a natural break ⇒ still changing.
    if iterations == max_iters {
        converged = false;
    }

    let unfixable = report
        .findings
        .iter()
        .filter(|f| !f.is_suppressed() && f.fix.is_none())
        .count();

    Fixpoint {
        sql: current,
        edits: first_edits,
        applied: applied_total,
        iterations,
        converged,
        original_findings,
        final_report: report,
        unfixable,
        skipped_overlapping,
        withheld,
    }
}

#[cfg(test)]
mod tests {
    use super::render_diff;
    use crate::{FileReport, Fix, FixEdit, Location, Severity};

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

    #[test]
    fn empty_edits_render_nothing() {
        assert_eq!(render_diff("f.sql", "SELECT 1;\n", &[]), "");
    }

    #[test]
    fn single_line_replacement_diff() {
        // "CREATE INDEX i ON t (c);\n" — insert " CONCURRENTLY" after "CREATE INDEX" (byte 12).
        // The full diff's byte-exactness is enforced by the `git apply` round-trip in
        // tests/fix_cli.rs; here we lock the structure the git format demands.
        let sql = "CREATE INDEX i ON t (c);\n";
        let edits = vec![FixEdit {
            start: 12,
            end: 12,
            replacement: " CONCURRENTLY".into(),
        }];
        let out = render_diff("f.sql", sql, &edits);
        // git-parseable headers (a/ b/ prefixes, no " (fixed)" suffix).
        assert!(
            out.starts_with("--- a/f.sql\n+++ b/f.sql\n"),
            "headers must use git a/ b/ prefixes: {out}"
        );
        assert!(out.contains("@@ "), "must have a hunk header: {out}");
        assert!(
            out.contains("-CREATE INDEX i ON t (c);\n"),
            "must remove the original line: {out}"
        );
        assert!(
            out.contains("+CREATE INDEX CONCURRENTLY i ON t (c);\n"),
            "must add the fixed line: {out}"
        );
    }

    #[test]
    fn newline_adding_replacement_grows_line_count() {
        // Prologue insertion at byte 0 adds a line before the statement, so the hunk
        // emits two `+` lines against one `-` line.
        let sql = "CREATE INDEX i ON t (c);\n";
        let edits = vec![FixEdit {
            start: 0,
            end: 0,
            replacement: "SET lock_timeout = '5s';\n".into(),
        }];
        let out = render_diff("f.sql", sql, &edits);
        assert!(out.starts_with("--- a/f.sql\n+++ b/f.sql\n"), "{out}");
        assert!(out.contains("@@ "), "{out}");
        assert!(out.contains("-CREATE INDEX i ON t (c);\n"), "{out}");
        assert!(out.contains("+SET lock_timeout = '5s';\n"), "{out}");
        assert!(out.contains("+CREATE INDEX i ON t (c);\n"), "{out}");
        // One line removed, two added.
        assert_eq!(
            out.lines()
                .filter(|l| l.starts_with('-') && !l.starts_with("---"))
                .count(),
            1
        );
        assert_eq!(
            out.lines()
                .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
                .count(),
            2
        );
    }

    #[test]
    fn nearby_changes_coalesce_with_context() {
        // Two changed lines (0 and 3) two unchanged lines apart coalesce into a single
        // hunk, with the intervening lines shown as context. `+` lines carry both fixes.
        let sql = "CREATE INDEX i ON a (c);\nSELECT 1;\nSELECT 2;\nCREATE INDEX j ON b (c);\n";
        let edits = vec![
            FixEdit {
                start: 0,
                end: 0,
                replacement: "SET lock_timeout = '5s';\n".into(),
            },
            FixEdit {
                // "CREATE INDEX" on line 3 starts at byte 45; insertion point is byte 57.
                start: 57,
                end: 57,
                replacement: " CONCURRENTLY".into(),
            },
        ];
        let out = render_diff("f.sql", sql, &edits);
        assert!(out.starts_with("--- a/f.sql\n+++ b/f.sql\n"), "{out}");
        // A single coalesced hunk (the 2-line gap is <= 2*CONTEXT).
        assert_eq!(
            out.matches("@@ ").count(),
            1,
            "expected one coalesced hunk: {out}"
        );
        // Intervening unchanged lines appear as context (space prefix).
        assert!(out.contains(" SELECT 1;\n"), "context line missing: {out}");
        assert!(out.contains(" SELECT 2;\n"), "context line missing: {out}");
        assert!(out.contains("+SET lock_timeout = '5s';\n"), "{out}");
        assert!(
            out.contains("+CREATE INDEX CONCURRENTLY j ON b (c);\n"),
            "{out}"
        );
    }

    #[test]
    fn no_trailing_newline_emits_marker() {
        // Final line lacks a trailing newline: both the removed and the added final
        // line must carry git's `\ No newline at end of file` marker.
        let sql = "CREATE INDEX i ON t (c);";
        let edits = vec![FixEdit {
            start: 12,
            end: 12,
            replacement: " CONCURRENTLY".into(),
        }];
        let out = render_diff("f.sql", sql, &edits);
        assert!(
            out.contains("\\ No newline at end of file"),
            "must mark the missing trailing newline: {out}"
        );
        // Marker appears for both the `-` and the `+` final line.
        assert_eq!(
            out.matches("\\ No newline at end of file").count(),
            2,
            "one marker per side: {out}"
        );
    }

    // --- fixpoint driver test helpers ---

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

    fn report_with(findings: Vec<crate::Finding>) -> FileReport {
        FileReport {
            name: "t".into(),
            findings,
            error: None,
        }
    }

    // Replace the single char at byte 0 with `to`.
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
        let fp = super::fix_to_fixpoint("t", "1", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "3");
        assert_eq!(fp.iterations, 2);
        assert!(fp.converged);
        assert!(fp.withheld.is_none());
        assert_eq!(fp.applied, 2);
        // Two applying passes → original-coordinate edits are no longer valid.
        assert!(fp.edits.is_none());
    }

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
        let fp = super::fix_to_fixpoint("t", "1", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "2");
        assert_eq!(fp.iterations, 1);
        assert!(fp.converged);
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

    #[test]
    fn fixpoint_no_fixes_is_a_noop() {
        let lint = |_: &str| report_with(vec![]);
        let fp = super::fix_to_fixpoint("t", "SELECT 1;", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "SELECT 1;");
        assert_eq!(fp.iterations, 0);
        assert!(fp.converged);
        assert!(fp.edits.is_none());
        assert_eq!(fp.applied, 0);
    }

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
        let fp = super::fix_to_fixpoint("t", "a", lint, 4);
        assert!(!fp.converged);
        assert_eq!(fp.iterations, 4);
        assert!(fp.withheld.is_none());
        // Result is one of the validated intermediates — always parse-valid SQL.
        assert!(fp.sql == "a" || fp.sql == "b");
    }

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
        let fp = super::fix_to_fixpoint("1", "1", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "2"); // kept the last validated-good result
        assert_eq!(fp.iterations, 1);
        assert!(matches!(fp.withheld, Some(super::Withheld::NewError)));
        assert!(fp.converged); // clean stop, not a cap overflow
    }

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
        let fp = super::fix_to_fixpoint("1", "1", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "1");
        assert_eq!(fp.iterations, 0);
        assert!(matches!(fp.withheld, Some(super::Withheld::NewError)));
        assert!(fp.edits.is_none());
    }

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
        let fp = super::fix_to_fixpoint("t", "ok", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "ok");
        assert!(matches!(fp.withheld, Some(super::Withheld::ParseBroke(_))));
    }

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
            let fp = super::fix_to_fixpoint("<test>", sql, lint, super::MAX_FIX_ITERATIONS);
            assert!(
                fp.iterations <= 1,
                "input unexpectedly cascaded ({} passes): {sql:?}",
                fp.iterations
            );
            assert!(fp.converged, "input did not converge: {sql:?}");
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
