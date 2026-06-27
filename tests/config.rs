use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

/// Run pgsafe from `dir` with `args`. Returns the assert for further checks.
fn run_in(dir: &std::path::Path, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .current_dir(dir)
        .args(args)
        .assert()
}

#[test]
fn disabling_a_rule_drops_its_findings() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join(".pgsafe.toml"),
        "[rules]\ndrop-table = false\n",
    )
    .unwrap();
    fs::write(dir.path().join("m.sql"), "DROP TABLE x;\n").unwrap();
    run_in(dir.path(), &["m.sql"]).stdout(predicate::str::contains("drop-table").not());
}

#[test]
fn severity_override_changes_gating() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("m.sql"), "CREATE INDEX i ON t (x);\n").unwrap();
    // add-index-non-concurrent is normally an error → --fail-on=error exits 1.
    run_in(dir.path(), &["--no-config", "--fail-on", "error", "m.sql"])
        .failure()
        .code(1);
    // Demoted to warning by config → --fail-on=error no longer fails.
    fs::write(
        dir.path().join(".pgsafe.toml"),
        "[rules]\nadd-index-non-concurrent = \"warning\"\n",
    )
    .unwrap();
    run_in(dir.path(), &["--fail-on", "error", "m.sql"]).success();
}

#[test]
fn per_path_ignore_applies_only_to_matching_files() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join(".pgsafe.toml"),
        "[[ignore]]\npath = \"legacy/**\"\nrules = [\"drop-table\"]\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("legacy")).unwrap();
    fs::write(dir.path().join("legacy/a.sql"), "DROP TABLE x;\n").unwrap();
    fs::write(dir.path().join("current.sql"), "DROP TABLE y;\n").unwrap();
    run_in(dir.path(), &["legacy/a.sql"]).stdout(predicate::str::contains("drop-table").not());
    run_in(dir.path(), &["current.sql"]).stdout(predicate::str::contains("drop-table"));
}

#[test]
fn no_config_ignores_a_present_file() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join(".pgsafe.toml"),
        "[rules]\ndrop-table = false\n",
    )
    .unwrap();
    fs::write(dir.path().join("m.sql"), "DROP TABLE x;\n").unwrap();
    // With the config, drop-table is gone; with --no-config it fires again.
    run_in(dir.path(), &["--no-config", "m.sql"]).stdout(predicate::str::contains("drop-table"));
}

#[test]
fn explicit_config_path_is_used() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("ci.toml"), "[rules]\ndrop-table = false\n").unwrap();
    fs::write(dir.path().join("m.sql"), "DROP TABLE x;\n").unwrap();
    run_in(dir.path(), &["--config", "ci.toml", "m.sql"])
        .stdout(predicate::str::contains("drop-table").not());
}

#[test]
fn malformed_config_fails_with_exit_2() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(".pgsafe.toml"), "fail_onn = \"error\"\n").unwrap();
    fs::write(dir.path().join("m.sql"), "DROP TABLE x;\n").unwrap();
    run_in(dir.path(), &["m.sql"]).failure().code(2);
}

#[test]
fn unknown_rule_id_in_config_fails_with_exit_2() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join(".pgsafe.toml"),
        "[rules]\ndrop-tabel = false\n",
    )
    .unwrap();
    fs::write(dir.path().join("m.sql"), "DROP TABLE x;\n").unwrap();
    run_in(dir.path(), &["m.sql"]).failure().code(2);
}

#[test]
fn directive_for_a_disabled_rule_is_not_unused() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join(".pgsafe.toml"),
        "[rules]\ndrop-table = false\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("m.sql"),
        "-- pgsafe:ignore drop-table  disabled anyway\nDROP TABLE x;\n",
    )
    .unwrap();
    run_in(dir.path(), &["m.sql"]).stdout(predicate::str::contains("suppression-unused").not());
}
