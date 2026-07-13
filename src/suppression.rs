//! Inline `-- pgsafe:ignore` suppression: parse directives from SQL comments,
//! attach them to statements, and resolve them against rule findings.

use std::collections::BTreeSet;

use crate::ast::protobuf::Token;

use crate::{line_col, Finding, LintError, Location, Severity, Suppression};

/// A parsed `pgsafe:` directive's payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DirectiveKind {
    /// A well-formed `pgsafe:ignore <rule-id> <reason>`. `reason` may be empty
    /// here; emptiness becomes `suppression-missing-reason` during resolution.
    Ignore { rule_id: String, reason: String },
    /// A `pgsafe:` comment that is not a usable `ignore` directive.
    Malformed { detail: &'static str },
}

/// Parse a comment token's raw text into a directive, or `None` if it is not a
/// `pgsafe:` directive at all.
pub(crate) fn parse_directive(token_text: &str) -> Option<DirectiveKind> {
    let inner = token_text.strip_prefix("--").map(str::trim).or_else(|| {
        token_text
            .strip_prefix("/*")
            .and_then(|s| s.strip_suffix("*/"))
            .map(str::trim)
    })?;
    let rest = inner.strip_prefix("pgsafe:")?.trim_start();
    let mut verb_split = rest.splitn(2, char::is_whitespace);
    let verb = verb_split.next().unwrap_or("");
    if verb != "ignore" {
        return Some(DirectiveKind::Malformed {
            detail: "unrecognized pgsafe directive (expected `ignore`)",
        });
    }
    let after = verb_split.next().unwrap_or("").trim_start();
    if after.is_empty() {
        return Some(DirectiveKind::Malformed {
            detail: "directive is missing a rule id",
        });
    }
    let mut id_split = after.splitn(2, char::is_whitespace);
    let rule_id = id_split.next().unwrap_or("").to_string();
    let reason = id_split.next().unwrap_or("").trim().to_string();
    Some(DirectiveKind::Ignore { rule_id, reason })
}

/// A comment token located in the source.
#[derive(Debug, Clone)]
pub(crate) struct Comment {
    /// Byte offset of the comment's first character within the SQL input.
    pub start: usize,
    /// 1-based source line the comment starts on.
    pub line: u32,
    /// Full text of the comment, including delimiters.
    pub text: String,
}

/// Extract every SQL/C comment token from `sql`, with byte offset and line.
pub(crate) fn scan_comments(sql: &str) -> Result<Vec<Comment>, LintError> {
    let scan = crate::ast::scan(sql).map_err(|e| LintError::Parse(e.to_string()))?;
    let mut out = Vec::new();
    for tok in &scan.tokens {
        if matches!(
            Token::try_from(tok.token),
            Ok(Token::SqlComment) | Ok(Token::CComment)
        ) {
            let start = usize::try_from(tok.start).unwrap_or(0);
            let end = usize::try_from(tok.end).unwrap_or(start);
            let text = sql.get(start..end).unwrap_or("").to_string();
            out.push(Comment {
                start,
                line: line_col(sql, start).0,
                text,
            });
        }
    }
    Ok(out)
}

/// A directive discovered in the source, with its location and original text.
#[derive(Debug, Clone)]
pub(crate) struct Directive {
    /// Parsed payload of the directive.
    pub kind: DirectiveKind,
    /// Source location of the directive comment.
    pub location: Location,
    /// Trimmed text of the directive comment.
    pub snippet: String,
    /// Byte offset of the directive comment's first character.
    pub start: usize,
}

/// Parse the `pgsafe:` directives out of a comment list (non-directives dropped).
pub(crate) fn directives_from(comments: &[Comment], sql: &str) -> Vec<Directive> {
    comments
        .iter()
        .filter_map(|c| {
            let kind = parse_directive(&c.text)?;
            let (line, column) = line_col(sql, c.start);
            Some(Directive {
                kind,
                location: Location {
                    byte: u32::try_from(c.start).unwrap_or(u32::MAX),
                    line,
                    column,
                },
                snippet: c.text.trim().to_string(),
                start: c.start,
            })
        })
        .collect()
}

/// Byte/line extent of one statement (first non-whitespace token to last).
#[derive(Debug, Clone)]
pub(crate) struct StatementGeom {
    /// 0-based index of the statement within the parsed statement list.
    pub index: usize,
    /// Byte offset of the statement's first real token (after skipping leading whitespace and comments).
    pub start: usize,
    /// Byte offset of the start of the statement's contiguous own-line leading comment block, or
    /// the statement's own line start when there is none. This is the correct anchor for inserting
    /// a statement-level prologue (e.g. `SET lock_timeout`) so the prologue lands ABOVE any
    /// own-line `-- pgsafe:ignore` directives rather than between a directive and the statement
    /// body. Computed by walking backward from `start`'s line over contiguous own-line comment
    /// lines. When there are no such lines, `prologue_anchor == line_start(sql, start)`.
    pub prologue_anchor: usize,
    /// Byte offset one past the statement's last non-whitespace character.
    pub end: usize,
    /// Byte offset one past the statement's last non-whitespace, non-comment character — i.e. `end`
    /// with any trailing comment(s) trimmed off. Equals `end` unless a comment sits between the
    /// statement's last real token and its terminator (the next `;`, or end-of-input for a final
    /// statement with no `;`): pg_query's `stmt_len` spans up to that terminator, folding the comment
    /// (and preceding whitespace) into the extent. This is the correct anchor for a `StatementBodyEnd`
    /// fix insertion, so the spliced text lands before a trailing comment rather than inside it.
    pub body_end: usize,
    /// 1-based line number of the statement's first real token.
    pub first_line: u32,
    /// 1-based line number of the statement's last non-whitespace character.
    pub last_line: u32,
}

/// Compute the trimmed byte/line extent of every statement.
///
/// pg_query may set `stmt_location = 0` for a statement that is preceded only
/// by comments (the comment is inside the extent), so we additionally skip any
/// leading SQL/C comments — not just ASCII whitespace — to find the true first
/// token of the statement.
pub(crate) fn geometry(
    sql: &str,
    stmts: &[crate::ast::protobuf::RawStmt],
    comments: &[Comment],
) -> Vec<StatementGeom> {
    // Pre-collect comment spans so we can skip them below.
    let comment_spans: Vec<(usize, usize)> = comments
        .iter()
        .map(|c| (c.start, c.start + c.text.len()))
        .collect();

    let mut out = Vec::with_capacity(stmts.len());
    for (index, raw) in stmts.iter().enumerate() {
        let off = usize::try_from(raw.stmt_location.max(0)).unwrap_or(0);
        let len = usize::try_from(raw.stmt_len.max(0)).unwrap_or(0);
        let raw_end = if len == 0 {
            sql.len()
        } else {
            off.saturating_add(len).min(sql.len())
        };
        let slice = sql.get(off..raw_end).unwrap_or("");
        let lead = slice.len() - slice.trim_start().len();
        let trail = slice.len() - slice.trim_end().len();
        let content_start = off + lead;
        // `start` advances past any leading comments to the statement's first real token.
        let start = skip_leading_comments(sql, content_start, &comment_spans);
        let end = raw_end.saturating_sub(trail).max(start);
        let body_end = trim_trailing_comments(sql, start, end, &comment_spans);
        let first_line = line_col(sql, start).0;
        let last_line = line_col(sql, end.saturating_sub(1).max(start)).0;
        // Walk backward from `start`'s line over any contiguous own-line comment lines so
        // that a statement-level prologue (e.g. SET lock_timeout) is inserted ABOVE any
        // own-line directives rather than between a directive and the statement body.
        let prologue_anchor = compute_prologue_anchor(sql, start, &comment_spans);
        out.push(StatementGeom {
            index,
            start,
            prologue_anchor,
            end,
            body_end,
            first_line,
            last_line,
        });
    }
    out
}

/// Back `end` up over any trailing comment(s) pg_query folded into the statement extent — which
/// happens whenever a comment precedes the statement's terminator (`;` or end-of-input), since
/// `stmt_len` spans up to that terminator and swallows a trailing `-- …` or `/* … */` (the no-`;`
/// last statement, whose extent runs to end-of-input, is one instance, not the only one). Trailing
/// whitespace between the statement body and the comment (and between stacked comments) is trimmed
/// too. Returns `end` unchanged when nothing trails the body.
fn trim_trailing_comments(
    sql: &str,
    start: usize,
    mut end: usize,
    spans: &[(usize, usize)],
) -> usize {
    loop {
        // `start..end` is always a valid char-boundary slice (both are token/whitespace boundaries
        // within the input), so `get` is `Some`; `new_end >= start` by construction.
        let trimmed_len = sql.get(start..end).map_or(0, |s| s.trim_end().len());
        let new_end = start + trimmed_len;
        // A comment that starts within the body and whose span reaches `new_end` means the body's
        // trailing bytes are that comment — drop it and re-trim the whitespace before it.
        match spans
            .iter()
            .find(|&&(cs, ce)| cs >= start && cs < new_end && ce >= new_end)
        {
            Some(&(cs, _)) => end = cs,
            None => return new_end,
        }
    }
}

/// Returns the byte offset of the first byte on the line containing `pos`.
fn line_start(sql: &str, pos: usize) -> usize {
    sql[..pos].rfind('\n').map_or(0, |i| i + 1)
}

/// Whether `sql[ls..le]` (a line's content, not including the trailing `\n`) is a
/// comment-only line: every non-whitespace byte lies inside a comment span. A
/// blank line returns `false`. This recognises `--` and single-line `/* */`
/// comments (whose first byte starts a span) as well as the continuation and
/// closing lines of a multi-line block comment (e.g. `   reason */`), whose bytes
/// fall inside the single span that opened on an earlier line — so the prologue
/// anchor walk does not stop partway through a block comment.
fn is_own_line_comment(sql: &str, ls: usize, le: usize, spans: &[(usize, usize)]) -> bool {
    let line = match sql.get(ls..le) {
        Some(s) => s,
        None => return false,
    };
    let ws = line.len() - line.trim_start().len();
    let first = ls + ws;
    // Blank line: no content after whitespace.
    if first >= le {
        return false;
    }
    // Byte of the last non-whitespace char on the line (used only for span
    // containment comparisons, so a multi-byte boundary is harmless).
    let last = ls + line.trim_end().len() - 1;
    let covered = |b: usize| spans.iter().any(|&(s, e)| s <= b && b < e);
    covered(first) && covered(last)
}

/// Compute the prologue-insertion anchor for the statement whose first token is at
/// `start`. Walk backward from `start`'s line over any contiguous own-line comment
/// lines; return the start of the earliest such line, or the line start of `start`
/// when there are no such lines directly above.
fn compute_prologue_anchor(sql: &str, start: usize, spans: &[(usize, usize)]) -> usize {
    let mut anchor = line_start(sql, start);
    loop {
        if anchor == 0 {
            break;
        }
        let above_end = anchor - 1; // byte of the '\n' that ends the line directly above
        let above_start = line_start(sql, above_end);
        if is_own_line_comment(sql, above_start, above_end, spans) {
            anchor = above_start;
        } else {
            break;
        }
    }
    anchor
}

/// Advance `pos` past any leading SQL/C comments (and whitespace between them).
fn skip_leading_comments(sql: &str, mut pos: usize, comments: &[(usize, usize)]) -> usize {
    loop {
        let maybe = comments.iter().find(|&&(s, _)| s == pos);
        let Some(&(_, end)) = maybe else { break };
        pos = end;
        let after = sql.get(pos..).unwrap_or("");
        pos += after.len() - after.trim_start().len();
    }
    pos
}

/// Attach each directive to a statement by line geometry (design §3): trailing on
/// the statement's last line, or in the contiguous comment run immediately above it.
pub(crate) fn attach(
    directives: &[Directive],
    geoms: &[StatementGeom],
    comment_lines: &BTreeSet<u32>,
) -> Vec<Option<usize>> {
    let mut code_lines: BTreeSet<u32> = BTreeSet::new();
    for g in geoms {
        for l in g.first_line..=g.last_line {
            code_lines.insert(l);
        }
    }
    directives
        .iter()
        .map(|d| {
            let dl = d.location.line;
            if let Some(g) = geoms
                .iter()
                .filter(|g| g.last_line == dl && g.end <= d.start)
                .max_by_key(|g| g.end)
            {
                return Some(g.index);
            }
            let g = geoms
                .iter()
                .filter(|g| g.first_line > dl)
                .min_by_key(|g| g.first_line)?;
            let contiguous = ((dl + 1)..g.first_line)
                .all(|l| comment_lines.contains(&l) && !code_lines.contains(&l));
            contiguous.then_some(g.index)
        })
        .collect()
}

/// Resolve directives against findings: mark suppressions and append hygiene diagnostics.
pub(crate) fn resolve(
    sql: &str,
    geoms: &[StatementGeom],
    comments: &[Comment],
    mut findings: Vec<Finding>,
    known_rule_ids: &[&'static str],
    new_table_dropped: &BTreeSet<usize>,
    disabled_rules: &BTreeSet<String>,
) -> Result<Vec<Finding>, LintError> {
    let comment_lines: BTreeSet<u32> = comments
        .iter()
        .flat_map(|c| {
            let span = u32::try_from(c.text.matches('\n').count()).unwrap_or(0);
            c.line..=c.line + span
        })
        .collect();
    let directives = directives_from(comments, sql);
    let attachment = attach(&directives, geoms, &comment_lines);
    let is_known = |id: &str| known_rule_ids.contains(&id);

    // Pass 1: apply suppressions, recording which directives matched a finding.
    let mut used = vec![false; directives.len()];
    for (di, dir) in directives.iter().enumerate() {
        let Some(stmt_idx) = attachment[di] else {
            continue;
        };
        if let DirectiveKind::Ignore { rule_id, reason } = &dir.kind {
            if is_known(rule_id) && !reason.is_empty() {
                let mut matched = false;
                for f in &mut findings {
                    if f.statement_index == stmt_idx && f.rule_id == *rule_id {
                        matched = true;
                        if f.suppression.is_none() {
                            f.suppression = Some(Suppression {
                                reason: reason.clone(),
                            });
                        }
                    }
                }
                used[di] = matched;
            }
        }
    }

    // Pass 2: synthesize hygiene diagnostics.
    let mut hygiene: Vec<Finding> = Vec::new();
    for (di, dir) in directives.iter().enumerate() {
        let stmt_idx = attachment[di].unwrap_or_else(|| fallback_index(geoms, dir.location.line));
        let diag: Option<(&'static str, Severity, String)> = match &dir.kind {
            DirectiveKind::Malformed { detail } => Some((
                "suppression-malformed",
                Severity::Error,
                (*detail).to_string(),
            )),
            DirectiveKind::Ignore { rule_id, reason } => {
                if !is_known(rule_id) {
                    Some((
                        "suppression-unknown-rule",
                        Severity::Error,
                        format!("directive targets unknown rule `{rule_id}`"),
                    ))
                } else if reason.is_empty() {
                    Some((
                        "suppression-missing-reason",
                        Severity::Error,
                        format!("directive for `{rule_id}` must include a reason"),
                    ))
                } else if !used[di]
                    && !new_table_dropped.contains(&stmt_idx)
                    && !disabled_rules.contains(rule_id.as_str())
                {
                    Some((
                        "suppression-unused",
                        Severity::Warning,
                        format!("directive for `{rule_id}` matched no finding"),
                    ))
                } else {
                    None
                }
            }
        };
        if let Some((id, severity, message)) = diag {
            hygiene.push(Finding {
                rule_id: id.to_string(),
                severity,
                message,
                guidance: hygiene_guidance(id),
                statement_index: stmt_idx,
                location: dir.location,
                snippet: dir.snippet.clone(),
                suppression: None,
                fix: None,
            });
        }
    }

    // Merge: statement source order; within a statement, findings (registry order)
    // precede hygiene (directive order). A stable sort preserves both because every
    // finding precedes every hygiene diagnostic in the pre-sort vec.
    findings.extend(hygiene);
    findings.sort_by_key(|f| f.statement_index);
    Ok(findings)
}

/// Statement index for a directive that attached to no statement: the nearest
/// following statement, else the last statement, else 0.
fn fallback_index(geoms: &[StatementGeom], line: u32) -> usize {
    geoms
        .iter()
        .find(|g| g.first_line >= line)
        .or_else(|| geoms.last())
        .map_or(0, |g| g.index)
}

/// Fix-it guidance for each hygiene diagnostic id.
fn hygiene_guidance(id: &str) -> String {
    match id {
        "suppression-malformed" => {
            "Use `-- pgsafe:ignore <rule-id> <reason>`; the only supported verb is `ignore`."
        }
        "suppression-unknown-rule" => {
            "Check the rule id against the documented rules; ids must match exactly."
        }
        "suppression-missing-reason" => {
            "Add a reason after the rule id so the override is auditable."
        }
        "suppression-unused" => {
            "Remove the directive — it no longer suppresses anything (the finding is gone)."
        }
        _ => "",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn parses_valid_line_directive() {
        assert_eq!(
            parse_directive("-- pgsafe:ignore drop-table  superseded by v2"),
            Some(DirectiveKind::Ignore {
                rule_id: "drop-table".into(),
                reason: "superseded by v2".into()
            })
        );
    }
    #[test]
    fn parses_block_comment_directive() {
        assert_eq!(
            parse_directive("/* pgsafe:ignore truncate  one-off */"),
            Some(DirectiveKind::Ignore {
                rule_id: "truncate".into(),
                reason: "one-off".into()
            })
        );
    }
    #[test]
    fn rule_id_without_reason_is_ignore_with_empty_reason() {
        assert_eq!(
            parse_directive("-- pgsafe:ignore drop-table"),
            Some(DirectiveKind::Ignore {
                rule_id: "drop-table".into(),
                reason: String::new()
            })
        );
    }
    #[test]
    fn no_rule_id_token_is_malformed() {
        assert!(matches!(
            parse_directive("-- pgsafe:ignore"),
            Some(DirectiveKind::Malformed { .. })
        ));
    }
    #[test]
    fn unknown_verb_is_malformed() {
        assert!(matches!(
            parse_directive("-- pgsafe:disable drop-table"),
            Some(DirectiveKind::Malformed { .. })
        ));
    }
    #[test]
    fn non_pgsafe_comment_is_not_a_directive() {
        assert_eq!(parse_directive("-- just a note"), None);
        assert_eq!(parse_directive("-- PGSAFE:IGNORE drop-table x"), None); // case-sensitive marker
    }

    #[test]
    fn scan_excludes_directive_text_inside_a_string_literal() {
        let sql = "SELECT '-- pgsafe:ignore drop-table x'; DROP TABLE y;";
        let comments = scan_comments(sql).unwrap();
        assert!(
            directives_from(&comments, sql).is_empty(),
            "string-literal content is not a directive"
        );
    }
    #[test]
    fn extracts_directive_with_position() {
        let sql = "-- pgsafe:ignore drop-table  reason here\nDROP TABLE x;";
        let dirs = directives_from(&scan_comments(sql).unwrap(), sql);
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].location.line, 1);
        assert!(
            matches!(&dirs[0].kind, DirectiveKind::Ignore { rule_id, .. } if rule_id == "drop-table")
        );
    }

    fn attach_map(sql: &str) -> Vec<Option<usize>> {
        let parsed = crate::ast::parse(sql).unwrap();
        let comments = scan_comments(sql).unwrap();
        let comment_lines: BTreeSet<u32> = comments.iter().map(|c| c.line).collect();
        let dirs = directives_from(&comments, sql);
        let geoms = geometry(sql, &parsed.protobuf.stmts, &comments);
        attach(&dirs, &geoms, &comment_lines)
    }

    #[test]
    fn preceding_directive_attaches_to_next_statement() {
        assert_eq!(
            attach_map("-- pgsafe:ignore drop-table  r\nDROP TABLE x;"),
            vec![Some(0)]
        );
    }
    #[test]
    fn preceding_through_a_plain_comment_still_attaches() {
        assert_eq!(
            attach_map("-- pgsafe:ignore drop-table  r\n-- a note\nDROP TABLE x;"),
            vec![Some(0)]
        );
    }
    #[test]
    fn blank_line_breaks_preceding_attachment() {
        assert_eq!(
            attach_map("-- pgsafe:ignore drop-table  r\n\nDROP TABLE x;"),
            vec![None]
        );
    }

    /// The `body_end`-bounded body text of each statement — what a `StatementBodyEnd` fix inserts
    /// after. Excludes any trailing comment folded into the extent.
    fn bodies(sql: &str) -> Vec<String> {
        let parsed = crate::ast::parse(sql).unwrap();
        let comments = scan_comments(sql).unwrap();
        geometry(sql, &parsed.protobuf.stmts, &comments)
            .iter()
            .map(|g| sql[g.start..g.body_end].to_string())
            .collect()
    }

    #[test]
    fn body_end_excludes_trailing_line_comment_without_semicolon() {
        assert_eq!(
            bodies("ALTER TABLE t ADD CHECK (a > 0) -- note"),
            vec!["ALTER TABLE t ADD CHECK (a > 0)"]
        );
    }

    #[test]
    fn body_end_excludes_comment_before_semicolon() {
        // The comment sits between the last token and `;`; pg_query's stmt_len spans to `;`, so the
        // comment is folded into the extent and must still be trimmed (not just the no-`;` case).
        assert_eq!(
            bodies("ALTER TABLE t ADD CHECK (a > 0) /* c */;"),
            vec!["ALTER TABLE t ADD CHECK (a > 0)"]
        );
    }

    #[test]
    fn body_end_peels_stacked_trailing_comments() {
        // The trim loop iterates: a block comment then a line comment, and a newline-separated pair.
        assert_eq!(
            bodies("ALTER TABLE t ADD CHECK (a > 0) /* a */ -- b"),
            vec!["ALTER TABLE t ADD CHECK (a > 0)"]
        );
        assert_eq!(
            bodies("ALTER TABLE t ADD CHECK (a > 0) -- a\n-- b"),
            vec!["ALTER TABLE t ADD CHECK (a > 0)"]
        );
    }

    #[test]
    fn body_end_scopes_trailing_comment_to_its_own_statement() {
        // A trailing comment on statement 0 must not bleed into statement 1's body_end (the
        // `cs >= start` lower bound), and statement 1's trailing comment is trimmed independently.
        assert_eq!(
            bodies("SELECT 1 -- lead\n; ALTER TABLE t ADD CHECK (a > 0) -- keep"),
            vec!["SELECT 1", "ALTER TABLE t ADD CHECK (a > 0)"]
        );
    }

    #[test]
    fn body_end_equals_end_when_nothing_trails_the_body() {
        // No trailing comment inside the extent (comment after `;`, or none): body_end == end.
        for sql in [
            "ALTER TABLE t ADD CHECK (a > 0);",
            "ALTER TABLE t ADD CHECK (a > 0); -- after the terminator",
        ] {
            let parsed = crate::ast::parse(sql).unwrap();
            let comments = scan_comments(sql).unwrap();
            for g in geometry(sql, &parsed.protobuf.stmts, &comments) {
                assert_eq!(g.body_end, g.end, "body_end must equal end for: {sql}");
            }
        }
    }
    #[test]
    fn trailing_directive_attaches_to_its_statement() {
        assert_eq!(
            attach_map("DROP TABLE x;  -- pgsafe:ignore drop-table  r"),
            vec![Some(0)]
        );
    }
    #[test]
    fn directive_routes_to_correct_statement_in_multi_statement_file() {
        assert_eq!(
            attach_map("DROP TABLE a;\n-- pgsafe:ignore drop-table  r\nDROP TABLE b;"),
            vec![Some(1)]
        );
    }
    #[test]
    fn stacked_directives_all_attach_to_following_statement() {
        let sql =
            "-- pgsafe:ignore drop-column  r1\n-- pgsafe:ignore drop-table  r2\nDROP TABLE x;";
        assert_eq!(attach_map(sql), vec![Some(0), Some(0)]);
    }

    fn resolved(sql: &str) -> Vec<Finding> {
        crate::lint_sql(sql, &crate::LintOptions::default()).unwrap()
    }
    fn ids(fs: &[Finding]) -> Vec<&str> {
        fs.iter().map(|f| f.rule_id.as_str()).collect()
    }

    #[test]
    fn valid_directive_suppresses_the_finding() {
        let fs = resolved("-- pgsafe:ignore drop-table  empty, confirmed\nDROP TABLE x;");
        let dt = fs.iter().find(|f| f.rule_id == "drop-table").unwrap();
        assert!(dt.is_suppressed());
        assert_eq!(dt.suppression.as_ref().unwrap().reason, "empty, confirmed");
        assert!(!ids(&fs).contains(&"suppression-unused"));
    }
    #[test]
    fn missing_reason_does_not_suppress_and_emits_diagnostic() {
        let fs = resolved("-- pgsafe:ignore drop-table\nDROP TABLE x;");
        assert!(!fs
            .iter()
            .find(|f| f.rule_id == "drop-table")
            .unwrap()
            .is_suppressed());
        assert!(ids(&fs).contains(&"suppression-missing-reason"));
    }
    #[test]
    fn unknown_rule_does_not_suppress_and_emits_diagnostic() {
        let fs = resolved("-- pgsafe:ignore drop-tabel  typo\nDROP TABLE x;");
        assert!(!fs
            .iter()
            .find(|f| f.rule_id == "drop-table")
            .unwrap()
            .is_suppressed());
        assert!(ids(&fs).contains(&"suppression-unknown-rule"));
    }
    #[test]
    fn unused_directive_emits_warning() {
        let fs = resolved("-- pgsafe:ignore truncate  stale\nDELETE FROM x;");
        let unused = fs
            .iter()
            .find(|f| f.rule_id == "suppression-unused")
            .unwrap();
        assert_eq!(unused.severity, Severity::Warning);
    }
    #[test]
    fn malformed_directive_emits_error_and_does_not_suppress() {
        let fs = resolved("-- pgsafe:disable drop-table\nDROP TABLE x;");
        let m = fs
            .iter()
            .find(|f| f.rule_id == "suppression-malformed")
            .unwrap();
        assert_eq!(m.severity, Severity::Error);
        assert!(!fs
            .iter()
            .find(|f| f.rule_id == "drop-table")
            .unwrap()
            .is_suppressed());
    }
    #[test]
    fn one_directive_suppresses_only_its_rule_on_a_multi_finding_statement() {
        let sql = "-- pgsafe:ignore drop-column  c retired\n\
                   ALTER TABLE t DROP COLUMN c, ADD COLUMN d int UNIQUE;";
        let fs = resolved(sql);
        assert!(fs
            .iter()
            .find(|f| f.rule_id == "drop-column")
            .unwrap()
            .is_suppressed());
        assert!(!fs
            .iter()
            .find(|f| f.rule_id == "add-unique-constraint")
            .unwrap()
            .is_suppressed());
    }
    #[test]
    fn returned_order_is_statement_then_hygiene() {
        let fs = resolved("DROP TABLE a;\n-- pgsafe:ignore truncate  stale\nDROP TABLE b;");
        // rule-loop findings precede engine-synthesized ones within each statement;
        // hygiene diagnostics follow all statement findings.
        assert_eq!(
            ids(&fs),
            vec![
                "drop-table",
                "require-timeout",
                "drop-table",
                "require-timeout",
                "suppression-unused",
            ]
        );
    }

    #[test]
    fn duplicate_directive_for_same_rule_is_not_unused() {
        let fs = crate::lint_sql(
            "-- pgsafe:ignore drop-table  reason one\nDROP TABLE x;  -- pgsafe:ignore drop-table  reason two",
            &crate::LintOptions::default(),
        )
        .unwrap();
        assert!(fs
            .iter()
            .find(|f| f.rule_id == "drop-table")
            .unwrap()
            .is_suppressed());
        assert!(!fs.iter().any(|f| f.rule_id == "suppression-unused"));
    }
    #[test]
    fn multiline_block_comment_directive_attaches() {
        let fs = crate::lint_sql(
            "/* pgsafe:ignore drop-table\n   confirmed empty */\nDROP TABLE x;",
            &crate::LintOptions::default(),
        )
        .unwrap();
        assert!(fs
            .iter()
            .find(|f| f.rule_id == "drop-table")
            .unwrap()
            .is_suppressed());
    }

    #[test]
    fn suppressed_finding_location_points_at_the_statement_not_the_directive() {
        let fs = crate::lint_sql(
            "-- pgsafe:ignore drop-table  confirmed empty\nDROP TABLE x;",
            &crate::LintOptions::default(),
        )
        .unwrap();
        let dt = fs.iter().find(|f| f.rule_id == "drop-table").unwrap();
        assert!(dt.is_suppressed());
        assert_eq!(
            dt.location.line, 2,
            "location must point at DROP (line 2), not the directive (line 1)"
        );
        // pg_query stmt_len excludes the trailing semicolon; the key invariant is that the
        // directive comment is NOT baked into the snippet.
        assert!(
            dt.snippet.starts_with("DROP TABLE"),
            "snippet must not include the directive comment; got: {:?}",
            dt.snippet
        );
        assert!(
            !dt.snippet.contains("pgsafe"),
            "snippet must not contain the directive text; got: {:?}",
            dt.snippet
        );
    }
}
