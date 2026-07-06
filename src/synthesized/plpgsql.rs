//! Recovery of statically-analyzable SQL from a `DO` block's PL/pgSQL body, via the experimental
//! `crate::ast::parse_plpgsql`. Pure: produces the embedded SQL statement texts plus a flag for
//! un-analyzable residue (a dynamic `EXECUTE`, or a body that would not parse). Not a rule — the
//! engine (`lint_sql`) replays the statements through the per-statement rules; the residue flag is
//! consumed by the `unchecked-do-block` rule (Task 3).
use serde_json::Value;

/// The result of statically analyzing a `DO` block body.
pub(crate) struct DoBodyAnalysis {
    /// SQL statement texts recovered from `PLpgSQL_stmt_execsql` nodes, in body order.
    pub statements: Vec<String>,
    /// `true` if the block holds anything pgsafe cannot statically analyze: a dynamic-SQL form
    /// (`EXECUTE`, `FOR … IN EXECUTE`, `OPEN … FOR EXECUTE`), or a body that `parse_plpgsql`
    /// rejected. Note: `lint_sql` may additionally mark residue when a recovered statement string
    /// fails SQL re-parse, so this flag captures only the plpgsql-level residue — not the only
    /// source of residue the caller tracks.
    pub has_residue: bool,
}

/// Recover the statically-analyzable statements from a `DO` block's source text. A body that does not
/// parse (e.g. a non-PL/pgSQL `DO … LANGUAGE`, or a malformed body) yields no statements and
/// `has_residue = true`.
pub(crate) fn analyze_do_block(do_sql: &str) -> DoBodyAnalysis {
    let Ok(json) = crate::ast::parse_plpgsql(do_sql) else {
        return DoBodyAnalysis {
            statements: Vec::new(),
            has_residue: true,
        };
    };
    let mut analysis = DoBodyAnalysis {
        statements: Vec::new(),
        has_residue: false,
    };
    walk(&json, &mut analysis);
    analysis
}

/// Recursively collect every `PLpgSQL_stmt_execsql` query text and flag any dynamic-SQL form
/// (`EXECUTE` / `FOR … IN EXECUTE` / `OPEN … FOR EXECUTE`) as residue. The plpgsql JSON nests
/// statement objects under control-flow keys (`then_body`, etc.), so a full descent reaches
/// statements inside `IF`/`LOOP`/`CASE`/nested blocks.
fn walk(value: &Value, out: &mut DoBodyAnalysis) {
    match value {
        Value::Object(map) => {
            if let Some(execsql) = map.get("PLpgSQL_stmt_execsql") {
                if let Some(query) = execsql
                    .get("sqlstmt")
                    .and_then(|n| n.get("PLpgSQL_expr"))
                    .and_then(|n| n.get("query"))
                    .and_then(Value::as_str)
                {
                    out.statements.push(query.to_string());
                } else {
                    // execsql node present but its query text could not be read — treat as residue
                    // rather than silently dropping a statement.
                    out.has_residue = true;
                }
            }
            if map.contains_key("PLpgSQL_stmt_dynexecute")
                || map.contains_key("PLpgSQL_stmt_dynfors")
                || map.contains_key("dynquery")
            {
                out.has_residue = true;
            }
            for child in map.values() {
                walk(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::analyze_do_block;

    #[test]
    fn recovers_a_single_direct_statement() {
        let a = analyze_do_block("DO $$ BEGIN ALTER TABLE t ADD COLUMN x int; END $$;");
        assert_eq!(a.statements.len(), 1);
        assert!(a.statements[0].contains("ALTER TABLE t"));
        assert!(!a.has_residue);
    }

    #[test]
    fn recovers_multiple_statements_in_body_order() {
        let a = analyze_do_block("DO $$ BEGIN CREATE INDEX i ON t (c); DROP TABLE u; END $$;");
        assert_eq!(a.statements.len(), 2);
        assert!(a.statements[0].contains("CREATE INDEX"));
        assert!(a.statements[1].contains("DROP TABLE"));
        assert!(!a.has_residue);
    }

    #[test]
    fn recovers_statements_nested_in_control_flow() {
        let a =
            analyze_do_block("DO $$ BEGIN IF true THEN CREATE INDEX i ON t (c); END IF; END $$;");
        assert_eq!(a.statements.len(), 1);
        assert!(a.statements[0].contains("CREATE INDEX"));
        assert!(!a.has_residue);
    }

    #[test]
    fn control_only_body_yields_no_statements_and_no_residue() {
        let a = analyze_do_block("DO $$ BEGIN PERFORM 1; END $$;");
        assert!(a.statements.is_empty());
        assert!(!a.has_residue);
    }

    #[test]
    fn dynamic_execute_is_residue() {
        let a = analyze_do_block("DO $$ BEGIN EXECUTE 'ALTER TABLE t ADD COLUMN x int'; END $$;");
        assert!(a.has_residue);
    }

    #[test]
    fn unparsable_body_is_residue_with_no_statements() {
        // missing END IF — parse_plpgsql rejects the body.
        let a = analyze_do_block("DO $$ BEGIN IF true THEN NULL; END $$;");
        assert!(a.has_residue);
        assert!(a.statements.is_empty());
    }

    #[test]
    fn mixed_execsql_and_execute_keeps_static_and_flags_residue() {
        let a = analyze_do_block(
            "DO $$ BEGIN ALTER TABLE t ADD COLUMN x int; EXECUTE 'DROP TABLE u'; END $$;",
        );
        assert_eq!(a.statements.len(), 1);
        assert!(a.statements[0].contains("ALTER TABLE t"));
        assert!(a.has_residue);
    }

    #[test]
    fn do_with_explicit_language_clause_recovers_statements() {
        let a = analyze_do_block(
            "DO LANGUAGE plpgsql $$ BEGIN ALTER TABLE t ADD COLUMN x int; END $$;",
        );
        assert_eq!(a.statements.len(), 1);
        assert!(a.statements[0].contains("ALTER TABLE t"));
        assert!(!a.has_residue);
    }

    #[test]
    fn for_in_execute_is_residue() {
        let a = analyze_do_block(
            "DO $$ DECLARE r record; BEGIN FOR r IN EXECUTE 'SELECT 1' LOOP NULL; END LOOP; END $$;",
        );
        assert!(a.has_residue);
    }

    #[test]
    fn open_for_execute_is_residue() {
        let a = analyze_do_block(
            "DO $$ DECLARE c refcursor; BEGIN OPEN c FOR EXECUTE 'SELECT 1'; END $$;",
        );
        assert!(a.has_residue);
    }
}
