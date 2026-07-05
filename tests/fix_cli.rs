use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

fn pgsafe() -> Command {
    Command::cargo_bin("pgsafe").unwrap()
}

#[test]
fn fix_rewrites_file_in_place() {
    let dir = tempdir().unwrap();
    let f = dir.path().join("m.sql");
    fs::write(&f, "CREATE INDEX i ON t (c);\n").unwrap();
    // require-timeout also fires; the index fix adds CONCURRENTLY.
    let _ = pgsafe().arg("--fix").arg(&f).assert();
    let after = fs::read_to_string(&f).unwrap();
    assert!(after.contains("CONCURRENTLY"), "got: {after}");
}

#[test]
fn fix_stdin_writes_fixed_sql_to_stdout() {
    pgsafe()
        .arg("--fix")
        .write_stdin("ALTER TABLE t ADD COLUMN c json;\n")
        .assert()
        .stdout(predicate::str::contains("jsonb"));
}

#[test]
fn diff_previews_without_writing() {
    let dir = tempdir().unwrap();
    let f = dir.path().join("m.sql");
    let before = "CREATE INDEX i ON t (c);\n";
    fs::write(&f, before).unwrap();
    pgsafe()
        .arg("--diff")
        .arg(&f)
        .assert()
        .stdout(predicate::str::contains("+CREATE INDEX CONCURRENTLY"));
    assert_eq!(
        fs::read_to_string(&f).unwrap(),
        before,
        "diff must not write"
    );
}

#[test]
fn fix_is_idempotent() {
    let dir = tempdir().unwrap();
    let f = dir.path().join("m.sql");
    fs::write(&f, "ALTER TABLE t ADD COLUMN c json;\n").unwrap();
    let _ = pgsafe().arg("--fix").arg(&f).assert();
    let once = fs::read_to_string(&f).unwrap();
    let _ = pgsafe().arg("--fix").arg(&f).assert();
    let twice = fs::read_to_string(&f).unwrap();
    assert_eq!(once, twice, "second --fix must be a no-op");
}

#[test]
fn fix_conflicts_with_diff() {
    pgsafe()
        .arg("--fix")
        .arg("--diff")
        .write_stdin("SELECT 1;")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn fix_conflicts_with_json_format() {
    pgsafe()
        .args(["--fix", "--format", "json"])
        .write_stdin("SELECT 1;")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn fix_does_not_touch_suppressed_findings() {
    let dir = tempdir().unwrap();
    let f = dir.path().join("m.sql");
    // A directive needs a non-empty reason to actually suppress (see
    // `src/suppression.rs`); without one it's `suppression-missing-reason` and
    // does not suppress, which would falsely fail this test.
    let src =
        "-- pgsafe:ignore add-index-non-concurrent  reviewed, acceptable here\nCREATE INDEX i ON t (c);\n";
    fs::write(&f, src).unwrap();
    let _ = pgsafe().arg("--fix").arg(&f).assert();
    let after = fs::read_to_string(&f).unwrap();
    assert!(
        !after.contains("CONCURRENTLY"),
        "suppressed finding must not be fixed: {after}"
    );
}

#[test]
fn fix_exit_reflects_post_fix_gate() {
    // A json column is fully fixable -> after fix, clean -> exit 0.
    let dir = tempdir().unwrap();
    let f = dir.path().join("clean.sql");
    fs::write(&f, "ALTER TABLE t ADD COLUMN c json;\n").unwrap();
    pgsafe().arg("--fix").arg(&f).assert().success();
}
