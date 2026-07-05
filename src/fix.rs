//! Auto-fix construction: a rule emits a [`FixDraft`] that expresses fix INTENT
//! as one or more anchored edits (absolute offsets, keyword positions, or
//! statement-relative anchors); the engine lowers each anchor to a validated
//! absolute byte [`crate::FixEdit`] using the source text and the statement's byte
//! span. A draft whose intent can't be located in the source resolves to `None`,
//! so an un-locatable fix is simply omitted rather than misapplied.

use crate::{Fix, FixEdit};

/// Where an edit attaches, in terms a rule can express without the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FixAnchor {
    /// Absolute byte span the rule computed itself (e.g. from a node `location`).
    Absolute { start: u32, end: u32 },
    /// Insert at the statement's first-token byte (`span.start`).
    #[allow(dead_code)] // Plan 2 producer: reserved for statement-prologue insertions
    StatementStart,
    /// Insert at the statement body's end (`span.end`, before any `;`).
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

/// Resolve a draft against the source. `start`/`end` are the statement's byte
/// span (`geoms[i].start/end`). Returns `None` if any anchor can't be located,
/// or if the draft carries no edits (upholding "fix present ⇒ at least one edit").
pub(crate) fn resolve(draft: &FixDraft, sql: &str, start: usize, end: usize) -> Option<Fix> {
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
            FixAnchor::StatementBodyEnd => (end, end),
            FixAnchor::AfterKeyword(kw) => {
                let at = keyword_end(sql.get(start..end)?, kw)? + start;
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

/// Apply a fix to `sql`, returning the rewritten string. Edits are applied high
/// offset to low so earlier splices don't shift later ones.
#[allow(dead_code)] // not yet wired to the CLI; called from tests only
pub(crate) fn apply(sql: &str, fix: &Fix) -> String {
    let mut out = sql.to_string();
    let mut edits = fix.edits.clone();
    edits.sort_by_key(|e| std::cmp::Reverse(e.start));
    for e in edits {
        out.replace_range(e.start as usize..e.end as usize, &e.replacement);
    }
    out
}

/// Outcome of composing a set of fixes onto one input.
#[allow(dead_code)] // wired to the CLI in Task 3
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
/// edits are collected into one merged [`Fix`] and spliced by the single-fix
/// [`apply`] primitive, which orders the splices high-to-low internally.
#[allow(dead_code)] // wired to the CLI in Task 3
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
    let merged = Fix {
        title: String::new(),
        edits: accepted.clone(),
    };
    // Reuse the single-fix splice primitive (it sorts descending internally).
    let sql = apply(sql, &merged);
    Applied {
        sql,
        edits: accepted,
        applied,
        skipped_overlapping,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(apply(SQL, &fix), "CREATE INDEX CONCURRENTLY i ON t (c);");
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
            apply(sql, &fix),
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
        assert_eq!(apply(SQL, &fix), "CREATE INDEX i ON t (c) NOT VALID;");
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
            apply(SQL, &fix),
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
        assert_eq!(apply(sql, &fix), "ALTER TABLE t ADD COLUMN c jsonb;");
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
        assert_eq!(apply("xyz", &fix), "AxyzB");
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
            apply(sql, &fix),
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
            apply(sql, &fix),
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
}
