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
/// stdin and all `--diff` output go to stdout. Fixes are driven to a fixpoint and
/// every accepted intermediate is re-linted (parses, introduces no new Error). If
/// a candidate no longer parses that's a tool bug: a hard error (exit 2) leaving
/// the file untouched and never echoing stdin — the pre-fixpoint behaviour. If a
/// candidate would instead introduce a new Error, only that further fix is backed
/// off: the safe fixes already made are still written/previewed and the run exits
/// on the gate. Exit: 2 on any parse/IO error; otherwise `--fix` gates on the final
/// re-lint, `--diff` on the original findings → 1 if gated findings remain, else 0.
/// When gating findings survive that no fix touched, a stderr note explains why
/// (never a silent nonzero exit).
pub(super) fn run(r: &ResolvedRun, mode: Mode) -> ExitCode {
    let mut had_error = false;
    let mut gated = false;

    for (name, sql) in &r.inputs {
        let lint = |s: &str| lint_input(name.clone(), s, &r.options_for(name));
        let fp = fix_to_fixpoint(sql, lint, MAX_FIX_ITERATIONS);

        // Original didn't parse: report exactly as before and move on.
        if let Some(err) = &fp.final_report.error {
            eprintln!("error: {name}: {err}");
            had_error = true;
            continue;
        }

        // A candidate that no longer parses is a tool bug, not a "findings remain"
        // outcome: hard error (exit 2), matching pre-fixpoint behaviour — leave the
        // file untouched and never echo stdin. Handled before the soft-withhold path
        // so `NewError` is the only back-off that writes/previews a partial result.
        if let Termination::Withheld(Withheld::ParseBroke(err)) = &fp.termination {
            let tail = match mode {
                Mode::Apply => "file left unchanged",
                Mode::Diff => "no diff shown",
            };
            eprintln!(
                "error: {name}: applying fixes produced SQL that no longer parses ({err}); {tail}"
            );
            had_error = true;
            continue;
        }

        let changed = fp.sql != *sql;

        // The soft back-off note: a further fix was withheld because applying it would
        // introduce a new Error. Only `NewError` reaches here (`ParseBroke` is the hard
        // error above), phrased by whether any safe fixes still landed. `verb` is the
        // mode's word for the changed case (Apply "wrote", Diff "previewing").
        let withheld_note = |verb: &str| -> Option<String> {
            matches!(fp.termination, Termination::Withheld(Withheld::NewError)).then(|| {
                if changed {
                    format!(
                        "{name}: some further fixes withheld — applying them would introduce a new issue; {verb} the safe ones"
                    )
                } else {
                    format!(
                        "{name}: fixes withheld — applying them would introduce a new issue; lint `{name}` to see the findings"
                    )
                }
            })
        };

        match mode {
            Mode::Diff => {
                if let Some(note) = withheld_note("previewing") {
                    eprintln!("{note}");
                    if !changed {
                        // Nothing safe to preview; gate on the original state (as today).
                        if gate(&fp.original_findings, r.fail_on) {
                            gated = true;
                        }
                        continue;
                    }
                }
                if matches!(fp.termination, Termination::CapHit) {
                    eprintln!(
                        "{name}: fixes did not converge after {} iterations; showing the last stable result",
                        fp.iterations
                    );
                }
                let diff = match &fp.edits {
                    Some(edits) => render_diff(name, sql, edits),
                    None => render_diff_strings(name, sql, &fp.sql),
                };
                print!("{diff}");
                // Diff gates on the original findings (it changes nothing on disk).
                if gate(&fp.original_findings, r.fail_on) {
                    gated = true;
                    let residual = fp.unfixable + fp.skipped_overlapping;
                    if residual > 0 {
                        eprintln!(
                            "{name}: {residual} finding(s) have no automatic fix or were skipped (overlapping another fix); lint `{name}` to see them"
                        );
                    }
                }
            }
            Mode::Apply => {
                // `fp.sql` is guaranteed parse-valid and regression-free; safe to write.
                if name == STDIN_NAME {
                    print!("{}", fp.sql);
                } else if changed {
                    if let Err(e) = write_atomic(name, &fp.sql) {
                        eprintln!("error: {name}: {e}");
                        had_error = true;
                        continue;
                    }
                }
                if let Some(note) = withheld_note("wrote") {
                    eprintln!("{note}");
                }
                if matches!(fp.termination, Termination::CapHit) {
                    // Gate the "wrote" phrasing on `changed`: an oscillation that returns
                    // to the original at the cap leaves the file untouched (nothing written).
                    let verb = if changed { "wrote" } else { "kept" };
                    eprintln!(
                        "{name}: fixes did not converge after {} iterations; {verb} the last stable result",
                        fp.iterations
                    );
                }
                if changed {
                    let mut note = String::new();
                    if fp.unfixable > 0 {
                        note.push_str(&format!("{} unfixable", fp.unfixable));
                    }
                    if fp.skipped_overlapping > 0 {
                        if !note.is_empty() {
                            note.push_str(", ");
                        }
                        note.push_str(&format!("{} skipped-overlapping", fp.skipped_overlapping));
                    }
                    let suffix = if note.is_empty() {
                        String::new()
                    } else {
                        format!(" ({note})")
                    };
                    eprintln!("fixed {} findings in {name}{suffix}", fp.applied);
                }
                // Apply gates on the post-fix re-lint.
                if gate(&fp.final_report.findings, r.fail_on) {
                    gated = true;
                    if !changed {
                        let remaining = fp
                            .final_report
                            .findings
                            .iter()
                            .filter(|f| !f.is_suppressed())
                            .count();
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

/// Coarse, dependency-free unified diff of two whole strings, used whenever
/// [`Fixpoint::edits`] is `None` — i.e. **0 or ≥2** applying passes (a single pass
/// keeps byte-exact edits for [`render_diff`]). The 0-pass/equal case has nothing
/// to show and returns `""`; the real work is the ≥2-pass case, where per-edit
/// offsets against the original are gone. Trims the common leading/trailing lines
/// and emits ONE hunk covering everything in between, surrounded by up to
/// [`CONTEXT`] context lines. Coarser than [`render_diff`]'s coalesced per-edit
/// hunks but correct and `git apply`-able.
fn render_diff_strings(name: &str, original: &str, updated: &str) -> String {
    if original == updated {
        return String::new();
    }
    let a: Vec<&str> = original.split_inclusive('\n').collect();
    let b: Vec<&str> = updated.split_inclusive('\n').collect();

    // Common leading lines.
    let mut pre = 0usize;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    // Common trailing lines (not re-counting the shared prefix).
    let mut suf = 0usize;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }

    let a_first = pre;
    let a_last_excl = a.len() - suf; // exclusive
    let b_first = pre;
    let b_last_excl = b.len() - suf;

    let ctx_before = pre.min(CONTEXT);
    let ctx_after = suf.min(CONTEXT);

    let old_start = a_first - ctx_before + 1; // 1-based
    let new_start = b_first - ctx_before + 1;
    let old_count = ctx_before + (a_last_excl - a_first) + ctx_after;
    let new_count = ctx_before + (b_last_excl - b_first) + ctx_after;

    let mut out = format!("--- a/{name}\n+++ b/{name}\n");
    out.push_str(&format!(
        "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
    ));
    for &line in &a[a_first - ctx_before..a_first] {
        push_diff_line(&mut out, ' ', line);
    }
    for &line in &a[a_first..a_last_excl] {
        push_diff_line(&mut out, '-', line);
    }
    for &line in &b[b_first..b_last_excl] {
        push_diff_line(&mut out, '+', line);
    }
    for &line in &a[a_last_excl..a_last_excl + ctx_after] {
        push_diff_line(&mut out, ' ', line);
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

/// Hard cap on fixpoint iterations. A deliberately generous bound: no real rule
/// cascades past a single applying pass today, so this ceiling is only ever reached
/// by a pathological or oscillating rule — the cap just lets such a rule stop
/// gracefully (last validated-good result kept) instead of looping forever.
const MAX_FIX_ITERATIONS: usize = 10;

/// A candidate the loop refused to accept, keeping the prior validated-good text.
enum Withheld {
    /// A candidate no longer parsed; its error message.
    ParseBroke(String),
    /// A candidate introduced a new Error vs the original.
    NewError,
}

/// How the fixpoint loop stopped. Exactly one of these holds per run, so the
/// withheld-note and non-convergence-note diagnostics are mutually exclusive by
/// construction (a contradictory pair is unrepresentable).
enum Termination {
    /// Reached a fixpoint: no remaining fix, or a pass made no textual progress.
    Converged,
    /// A candidate was backed off (see [`Withheld`]); prior good text is kept.
    Withheld(Withheld),
    /// Hit the iteration cap while still making textual progress.
    CapHit,
}

/// Result of driving `apply_all` to a fixpoint over one input.
struct Fixpoint {
    /// Final text: parse-valid and regression-free vs the original (may equal it)
    /// whenever `final_report.error` is `None`; otherwise (the input itself didn't
    /// parse) it equals the unparseable original and is unused.
    sql: String,
    /// `Some(edits-against-original)` iff **exactly one** applying pass occurred (the
    /// universal case today whenever any fix applies) — lets `--diff` reuse the
    /// byte-exact edit renderer. `None` when zero passes apply (nothing changed) or
    /// ≥2 do (offsets have shifted; use an original-vs-final diff).
    edits: Option<Vec<FixEdit>>,
    /// Total fixes applied across all passes.
    applied: usize,
    /// Applying passes performed.
    iterations: usize,
    /// How the loop stopped (fixpoint, back-off, or cap) — see [`Termination`].
    termination: Termination,
    /// The original input's findings (regression baseline; also the `--diff` gate basis).
    original_findings: Vec<Finding>,
    /// Re-lint of `sql` (drives the `--fix` exit gate and the unfixable count).
    final_report: FileReport,
    /// Non-suppressed findings in `final_report` with no automatic fix.
    unfixable: usize,
    /// Residual overlap-skips in the last applying pass.
    skipped_overlapping: usize,
}

/// Drive `apply_all` to a fixpoint. `lint` is injected so the loop is testable
/// without CLI wiring and can be exercised with a simulated cascade no real rule
/// produces yet. Every accepted intermediate is validated (parses, introduces no
/// new Error vs `original`), so `sql` is always safe to write.
fn fix_to_fixpoint(
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
        let fp = super::fix_to_fixpoint("1", lint, super::MAX_FIX_ITERATIONS);
        assert_eq!(fp.sql, "3");
        assert_eq!(fp.iterations, 2);
        assert!(matches!(fp.termination, super::Termination::Converged));
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

    #[test]
    fn render_diff_strings_single_hunk_between_common_context() {
        let a = "keep 1;\nOLD a;\nOLD b;\nkeep 2;\n";
        let b = "keep 1;\nNEW a;\nkeep 2;\n";
        let out = super::render_diff_strings("f.sql", a, b);
        assert!(out.starts_with("--- a/f.sql\n+++ b/f.sql\n"), "{out}");
        assert!(out.contains("@@ "), "{out}");
        assert!(out.contains(" keep 1;\n"), "leading context: {out}");
        assert!(out.contains(" keep 2;\n"), "trailing context: {out}");
        assert!(
            out.contains("-OLD a;\n") && out.contains("-OLD b;\n"),
            "removed lines: {out}"
        );
        assert!(out.contains("+NEW a;\n"), "added line: {out}");
    }

    #[test]
    fn render_diff_strings_equal_is_empty() {
        assert_eq!(super::render_diff_strings("f.sql", "x;\n", "x;\n"), "");
    }

    #[test]
    fn render_diff_strings_change_on_first_line() {
        // Change on the FIRST line: no common prefix (pre == 0 → ctx_before == 0), so
        // the hunk starts at line 1 with no leading context ahead of the change.
        let a = "OLD;\nkeep;\n";
        let b = "NEW;\nkeep;\n";
        let out = super::render_diff_strings("f.sql", a, b);
        assert!(out.contains("@@ -1,2 +1,2 @@\n"), "{out}");
        assert!(out.contains("-OLD;\n"), "{out}");
        assert!(out.contains("+NEW;\n"), "{out}");
        assert!(out.contains(" keep;\n"), "trailing context: {out}");
        // The change is the very first body line — nothing precedes it.
        assert!(
            out.starts_with("--- a/f.sql\n+++ b/f.sql\n@@ -1,2 +1,2 @@\n-OLD;\n"),
            "no leading context before a first-line change: {out}"
        );
    }

    #[test]
    fn render_diff_strings_change_reaching_last_line() {
        // Change reaching the LAST line: no common suffix (suf == 0 → ctx_after == 0),
        // so the hunk ends at the changed line with no trailing context after it.
        let a = "keep;\nOLD;\n";
        let b = "keep;\nNEW;\n";
        let out = super::render_diff_strings("f.sql", a, b);
        assert!(out.contains("@@ -1,2 +1,2 @@\n"), "{out}");
        assert!(out.contains(" keep;\n"), "leading context: {out}");
        assert!(out.contains("-OLD;\n"), "{out}");
        assert!(out.contains("+NEW;\n"), "{out}");
        // The added line is the end of output — no trailing context follows.
        assert!(
            out.ends_with("+NEW;\n"),
            "no trailing context after a last-line change: {out}"
        );
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
