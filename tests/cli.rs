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
    assert_eq!(fnd["severity"], "error");
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
    // human: a statement header ("{name}:{line}:{col}  {snippet}") then a nested
    // "  {severity} [{rule_id}]" line per finding.
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
        // SET lock_timeout keeps require-timeout from firing; only drop-table fires (suppressed).
        .write_stdin(
            "SET lock_timeout = '5s';\n-- pgsafe:ignore drop-table  empty, confirmed\nDROP TABLE x;",
        )
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
        // SET lock_timeout keeps require-timeout from firing; only drop-table fires (suppressed).
        .write_stdin(
            "SET lock_timeout = '5s';\n-- pgsafe:ignore drop-table  empty, confirmed\nDROP TABLE x;",
        )
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

// ── --fail-on gating ─────────────────────────────────────────────────────────

#[test]
fn fail_on_default_gates_on_a_warning() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("DROP TABLE x;")
        .assert()
        .failure()
        .code(1);
}

#[test]
fn fail_on_error_does_not_gate_on_a_warning() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "error"])
        .write_stdin("DROP TABLE x;")
        .assert()
        .success()
        .stdout(predicate::str::contains("drop-table")); // still printed, just not gating
}

#[test]
fn fail_on_error_gates_on_an_error() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "error"])
        .write_stdin("VACUUM FULL t;")
        .assert()
        .failure()
        .code(1);
}

#[test]
fn fail_on_never_gates_on_nothing() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "never"])
        .write_stdin("VACUUM FULL t;")
        .assert()
        .success()
        .stdout(predicate::str::contains("vacuum-full-cluster"));
}

#[test]
fn fail_on_never_still_exits_2_on_parse_error() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "never"])
        .write_stdin("ALTER TABLE;")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn invalid_fail_on_value_is_a_usage_error() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "bogus"])
        .write_stdin("DROP TABLE x;")
        .assert()
        .failure()
        .code(2);
}

// ── --in-transaction flag ────────────────────────────────────────────────────

#[test]
fn in_transaction_flag_flags_top_level_concurrently() {
    // Without the flag: a top-level CONCURRENTLY index is NOT a concurrently-in-transaction finding.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("CREATE INDEX CONCURRENTLY i ON t (x);")
        .assert()
        .stdout(predicate::str::contains("concurrently-in-transaction").not());

    // With --in-transaction: it IS flagged (exit 1).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--in-transaction"])
        .write_stdin("CREATE INDEX CONCURRENTLY i ON t (x);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("concurrently-in-transaction"));
}

// ── --fail-on × suppression-diagnostic gating ───────────────────────────────

#[test]
fn fail_on_error_gates_on_a_hygiene_error_but_not_on_unused() {
    // A reasonless directive emits suppression-missing-reason (error) → gates under --fail-on=error.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "error"])
        .write_stdin("-- pgsafe:ignore drop-table\nDROP TABLE x;")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("suppression-missing-reason"));
    // A stale directive emits suppression-unused (warning) → does NOT gate under --fail-on=error.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "error"])
        .write_stdin("-- pgsafe:ignore truncate  stale\nDELETE FROM x;")
        .assert()
        .success()
        .stdout(predicate::str::contains("suppression-unused"));
}

#[test]
fn require_primary_key_enabled_via_config_fires() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("pgsafe.toml");
    std::fs::write(&cfg, "[rules]\nrequire-primary-key = true\n").unwrap();

    // Without the config: no finding (off by default).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--no-config")
        .write_stdin("CREATE TABLE t (id int);")
        .assert()
        .success()
        .stdout(predicate::str::contains("require-primary-key").not());

    // With the config enabling it: the finding appears and gates (exit 1).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--config", cfg.to_str().unwrap()])
        .write_stdin("CREATE TABLE t (id int);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("require-primary-key"));
}

#[test]
fn forbidden_column_type_via_config_fires() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("pgsafe.toml");
    std::fs::write(&cfg, "[forbidden-types]\ntimestamp = \"timestamptz\"\n").unwrap();

    // Without the config: no finding.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--no-config")
        .write_stdin("CREATE TABLE t (created timestamp);")
        .assert()
        .success()
        .stdout(predicate::str::contains("forbidden-column-type").not());

    // With the config: the finding appears and gates (exit 1).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--config", cfg.to_str().unwrap()])
        .write_stdin("CREATE TABLE t (created timestamp);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("forbidden-column-type"));
}

#[test]
fn require_not_null_enabled_via_config_fires() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("pgsafe.toml");
    std::fs::write(&cfg, "[rules]\nrequire-not-null = true\n").unwrap();

    // Without the config: no finding (off by default).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--no-config")
        .write_stdin("CREATE TABLE t (email text);")
        .assert()
        .success()
        .stdout(predicate::str::contains("require-not-null").not());

    // With the config enabling it: the finding appears and gates (exit 1).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--config", cfg.to_str().unwrap()])
        .write_stdin("CREATE TABLE t (email text);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("require-not-null"));
}

#[test]
fn version_flag_prints_the_crate_version() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn require_columns_via_config_fires() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("pgsafe.toml");
    std::fs::write(&cfg, "required-columns = [\"created_at\"]\n").unwrap();

    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--no-config")
        .write_stdin("CREATE TABLE t (id int);")
        .assert()
        .success()
        .stdout(predicate::str::contains("require-columns").not());

    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--config", cfg.to_str().unwrap()])
        .write_stdin("CREATE TABLE t (id int);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("require-columns"));
}

#[test]
fn naming_convention_via_config_fires() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("pgsafe.toml");
    std::fs::write(&cfg, "[naming]\ntable = \"^t_\"\n").unwrap();

    // Without the config: no finding.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--no-config")
        .write_stdin("CREATE TABLE users (id int);")
        .assert()
        .success()
        .stdout(predicate::str::contains("naming-convention").not());

    // With the [naming] config: a mismatching table name is flagged and gates (exit 1).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--config", cfg.to_str().unwrap()])
        .write_stdin("CREATE TABLE users (id int);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("naming-convention"));
}
