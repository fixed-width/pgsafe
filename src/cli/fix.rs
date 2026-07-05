//! Fix-mode CLI: apply fixes (`--fix`) or preview them as a unified diff (`--diff`).

use std::process::ExitCode;

use super::ResolvedRun;
use crate::{gate, lint_input, Fix, FixEdit};

/// Stdin inputs carry this display-name (see `cli::mod::read_inputs`).
const STDIN_NAME: &str = "<stdin>";

/// Which fix-mode operation to run.
pub(super) enum Mode {
    /// Apply fixes: rewrite files in place; stdin → fixed SQL on stdout.
    Apply,
    /// Preview fixes as a unified diff; write nothing.
    Diff,
}

/// Run fix mode over the resolved inputs. Summaries go to stderr; `--fix` on
/// stdin and all `--diff` output go to stdout. Exit: 2 on any parse/IO error;
/// otherwise `--fix` gates on the post-fix re-lint, `--diff` on the original
/// findings → 1 if gated findings remain, else 0.
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
                print!("{}", render_diff(name, sql, &applied.edits));
                if gate(&report.findings, r.fail_on) {
                    gated = true;
                }
            }
            Mode::Apply => {
                let changed = applied.sql != *sql;
                if name == STDIN_NAME {
                    print!("{}", applied.sql);
                } else if changed {
                    if let Err(e) = std::fs::write(name, &applied.sql) {
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
                // Exit gate: re-lint the (possibly rewritten) text.
                let after = lint_input(name.clone(), &applied.sql, &r.options_for(name));
                if after.error.is_some() {
                    had_error = true;
                } else if gate(&after.findings, r.fail_on) {
                    gated = true;
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

/// 0-based line index containing byte offset `off` (clamped to the last line).
/// `line_starts` are the byte offsets of each line's first character.
fn line_of(line_starts: &[usize], off: usize) -> usize {
    match line_starts.binary_search(&off) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    }
}

/// Render `edits` against `original` as a unified diff. Edits (ascending,
/// non-overlapping) are grouped into hunks by the original lines they touch;
/// each hunk shows the touched original lines (`-`) and the spliced result (`+`).
pub(super) fn render_diff(name: &str, original: &str, edits: &[FixEdit]) -> String {
    if edits.is_empty() {
        return String::new();
    }
    // Line start offsets (line 0 begins at byte 0).
    let mut line_starts = vec![0usize];
    for (i, b) in original.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    // Group edits whose touched original-line ranges overlap or are contiguous.
    struct Group {
        first_line: usize,
        last_line: usize,
        edits: Vec<FixEdit>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for e in edits {
        let start_line = line_of(&line_starts, e.start as usize);
        // end offset is exclusive; the last touched line is the one containing end-1
        // (or the start line for a zero-width insertion).
        let end_byte = (e.end as usize).max(e.start as usize + 1) - 1;
        let end_line = line_of(&line_starts, end_byte);
        match groups.last_mut() {
            Some(g) if start_line <= g.last_line + 1 => {
                g.last_line = g.last_line.max(end_line);
                g.edits.push(e.clone());
            }
            _ => groups.push(Group {
                first_line: start_line,
                last_line: end_line,
                edits: vec![e.clone()],
            }),
        }
    }

    let mut out = format!("--- {name}\n+++ {name} (fixed)\n");
    let mut new_line_delta: isize = 0;
    for g in &groups {
        let block_start = line_starts[g.first_line];
        let block_end = line_starts
            .get(g.last_line + 1)
            .copied()
            .unwrap_or(original.len());
        let old_block = &original[block_start..block_end];

        // Splice this group's edits into the block (offsets local to block_start).
        let mut new_block = old_block.to_string();
        let mut local: Vec<&FixEdit> = g.edits.iter().collect();
        local.sort_by_key(|e| e.start);
        for e in local.iter().rev() {
            let s = e.start as usize - block_start;
            let en = e.end as usize - block_start;
            new_block.replace_range(s..en, &e.replacement);
        }

        let old_lines: Vec<&str> = old_block.split_inclusive('\n').collect();
        let new_lines: Vec<&str> = new_block.split_inclusive('\n').collect();
        let old_start = g.first_line + 1; // 1-based
        let new_start =
            usize::try_from(isize::try_from(old_start).unwrap() + new_line_delta).unwrap();
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            old_start,
            old_lines.len(),
            new_start,
            new_lines.len()
        ));
        for l in &old_lines {
            out.push_str(&format!("-{}", ensure_nl(l)));
        }
        for l in &new_lines {
            out.push_str(&format!("+{}", ensure_nl(l)));
        }
        new_line_delta += new_lines.len() as isize - old_lines.len() as isize;
    }
    out
}

/// Append a newline if the slice doesn't already end with one (last line of a
/// file with no trailing newline), so each diff line is newline-terminated.
fn ensure_nl(s: &str) -> String {
    if s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{s}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::render_diff;
    use crate::FixEdit;

    #[test]
    fn empty_edits_render_nothing() {
        assert_eq!(render_diff("f.sql", "SELECT 1;\n", &[]), "");
    }

    #[test]
    fn single_line_replacement_diff() {
        // "CREATE INDEX i ON t (c);\n" — insert " CONCURRENTLY" after "CREATE INDEX" (byte 12).
        let sql = "CREATE INDEX i ON t (c);\n";
        let edits = vec![FixEdit {
            start: 12,
            end: 12,
            replacement: " CONCURRENTLY".into(),
        }];
        let out = render_diff("f.sql", sql, &edits);
        assert_eq!(
            out,
            "--- f.sql\n\
             +++ f.sql (fixed)\n\
             @@ -1,1 +1,1 @@\n\
             -CREATE INDEX i ON t (c);\n\
             +CREATE INDEX CONCURRENTLY i ON t (c);\n"
        );
    }

    #[test]
    fn newline_adding_replacement_grows_line_count() {
        // Prologue insertion at byte 0 adds a line before the statement.
        let sql = "CREATE INDEX i ON t (c);\n";
        let edits = vec![FixEdit {
            start: 0,
            end: 0,
            replacement: "SET lock_timeout = '5s';\n".into(),
        }];
        let out = render_diff("f.sql", sql, &edits);
        assert_eq!(
            out,
            "--- f.sql\n\
             +++ f.sql (fixed)\n\
             @@ -1,1 +1,2 @@\n\
             -CREATE INDEX i ON t (c);\n\
             +SET lock_timeout = '5s';\n\
             +CREATE INDEX i ON t (c);\n"
        );
    }

    #[test]
    fn two_hunks_running_delta_shifts_second_hunk_new_start() {
        // Lines: 0 "CREATE INDEX i ON a (c);", 1 "SELECT 1;", 2 "SELECT 2;",
        //        3 "CREATE INDEX j ON b (c);". "CREATE INDEX" on line 3 starts at byte 45,
        //        so the insertion point after it is byte 57.
        let sql = "CREATE INDEX i ON a (c);\nSELECT 1;\nSELECT 2;\nCREATE INDEX j ON b (c);\n";
        let edits = vec![
            FixEdit {
                start: 0,
                end: 0,
                replacement: "SET lock_timeout = '5s';\n".into(),
            },
            FixEdit {
                start: 57,
                end: 57,
                replacement: " CONCURRENTLY".into(),
            },
        ];
        let out = render_diff("f.sql", sql, &edits);
        assert_eq!(
            out,
            "--- f.sql\n\
             +++ f.sql (fixed)\n\
             @@ -1,1 +1,2 @@\n\
             -CREATE INDEX i ON a (c);\n\
             +SET lock_timeout = '5s';\n\
             +CREATE INDEX i ON a (c);\n\
             @@ -4,1 +5,1 @@\n\
             -CREATE INDEX j ON b (c);\n\
             +CREATE INDEX CONCURRENTLY j ON b (c);\n"
        );
    }
}
