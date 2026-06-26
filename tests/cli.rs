use assert_cmd::Command;
use predicates::prelude::*;

// ── existing tests ───────────────────────────────────────────────────────────

#[test]
fn flags_add_index_non_concurrent_from_stdin() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("CREATE INDEX i ON t (x);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("add-index-non-concurrent"));
}

#[test]
fn clean_sql_succeeds() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("CREATE INDEX CONCURRENTLY i ON t (x);")
        .assert()
        .success();
}

#[test]
fn invalid_sql_exits_2() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("ALTER TABLE;")
        .assert()
        .code(2);
}

// ── file input ───────────────────────────────────────────────────────────────

#[test]
fn file_input_flags_findings_and_prints_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("migration.sql");
    std::fs::write(&path, "CREATE INDEX i ON t (x);").unwrap();

    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg(path.to_str().unwrap())
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("add-index-non-concurrent"))
        .stdout(predicate::str::contains(path.to_str().unwrap()));
}

// ── file not found ───────────────────────────────────────────────────────────

#[test]
fn file_not_found_exits_2_with_filename_in_stderr() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("no_such_file.sql")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no_such_file.sql"));
}

// ── parse error on stderr ────────────────────────────────────────────────────

#[test]
fn parse_error_exits_2_with_parse_error_in_stderr() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("ALTER TABLE;")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parse error"));
}

// ── clean SQL produces empty stdout ─────────────────────────────────────────

#[test]
fn clean_sql_produces_empty_stdout() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("CREATE INDEX CONCURRENTLY i ON t (x);")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

// ── multiple findings ────────────────────────────────────────────────────────

#[test]
fn multiple_findings_all_appear_in_output() {
    let output = Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("CREATE INDEX a ON t (x); CREATE INDEX b ON t (y);")
        .assert()
        .failure()
        .code(1)
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let count = stdout.matches("add-index-non-concurrent").count();
    assert_eq!(count, 2, "expected two findings, got {count} in:\n{stdout}");
}

// ── JSON structure ────────────────────────────────────────────────────────────

#[test]
fn json_format_structure_is_correct() {
    let out = Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--format", "json"])
        .write_stdin("CREATE INDEX i ON t (x);")
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(1));

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be valid JSON");

    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["files"][0]["file"], "<stdin>");

    let fnd = &v["files"][0]["findings"][0];
    assert_eq!(fnd["rule_id"], "add-index-non-concurrent");
    assert_eq!(fnd["severity"], "warning");
    assert!(fnd["location"]["line"].is_number(), "line must be a number");
    assert!(
        fnd["location"]["column"].is_number(),
        "column must be a number"
    );
    assert!(
        fnd["snippet"].as_str().unwrap().contains("CREATE INDEX"),
        "snippet must contain CREATE INDEX"
    );
}

#[test]
fn json_format_clean_sql_exits_0_with_empty_findings() {
    let out = Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--format", "json"])
        .write_stdin("CREATE INDEX CONCURRENTLY i ON t (x);")
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0));

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be valid JSON");

    let findings = v["files"][0]["findings"].as_array().unwrap();
    assert!(
        findings.is_empty(),
        "expected no findings, got {findings:?}"
    );
}

// ── error severity rendering ─────────────────────────────────────────────────

#[test]
fn error_severity_renders_in_human_and_json() {
    // human: format is "{name}: {severity} [{rule_id}] statement #…"
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("VACUUM FULL t;")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("error [vacuum-full-cluster]"));
    // json
    let out = Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--format", "json"])
        .write_stdin("VACUUM FULL t;")
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["files"][0]["findings"][0]["severity"], "error");
}

// ── suppression ──────────────────────────────────────────────────────────────

#[test]
fn suppressed_only_run_exits_zero() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("-- pgsafe:ignore drop-table  empty, confirmed\nDROP TABLE x;")
        .assert()
        .success()
        .stdout(predicate::str::contains("suppressed"));
}
#[test]
fn hygiene_diagnostic_gates_exit_one() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("-- pgsafe:ignore drop-table\nDROP TABLE x;")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("suppression-missing-reason"));
}
#[test]
fn json_output_includes_suppression_reason() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--format", "json"])
        .write_stdin("-- pgsafe:ignore drop-table  empty, confirmed\nDROP TABLE x;")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"suppression\""))
        .stdout(predicate::str::contains("empty, confirmed"));
}

// ── old JSON substring test (kept for backwards compat) ──────────────────────

#[test]
fn json_format_emits_rule_id_and_file() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--format", "json"])
        .write_stdin("CREATE INDEX i ON t (x);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("\"add-index-non-concurrent\""))
        .stdout(predicate::str::contains("\"file\""));
}
