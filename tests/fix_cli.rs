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
    // require-timeout also fires; both are fixable, so the run ends clean (exit 0).
    pgsafe().arg("--fix").arg(&f).assert().success();
    let after = fs::read_to_string(&f).unwrap();
    assert!(after.contains("CONCURRENTLY"), "got: {after}");
}

#[test]
fn fix_stdin_writes_fixed_sql_to_stdout() {
    // A json column is fully fixable, so the stdin run also ends clean (exit 0).
    pgsafe()
        .arg("--fix")
        .write_stdin("ALTER TABLE t ADD COLUMN c json;\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("jsonb"));
}

#[test]
fn diff_previews_without_writing() {
    let dir = tempdir().unwrap();
    let f = dir.path().join("m.sql");
    let before = "CREATE INDEX i ON t (c);\n";
    fs::write(&f, before).unwrap();
    // add-index-non-concurrent is an error, so --diff gates to exit 1 on the
    // ORIGINAL findings while writing nothing.
    pgsafe()
        .arg("--diff")
        .arg(&f)
        .assert()
        .failure()
        .code(1)
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

#[test]
fn fix_conflicts_with_github_format() {
    pgsafe()
        .args(["--fix", "--format", "github"])
        .write_stdin("SELECT 1;")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn fix_partial_leaves_unfixable_and_exits_1() {
    // The index line is fixable (CONCURRENTLY); DROP TABLE has no fix, so a gating
    // finding survives -> exit 1, and the DROP line is left byte-for-byte intact.
    let dir = tempdir().unwrap();
    let f = dir.path().join("partial.sql");
    fs::write(&f, "CREATE INDEX i ON t (c);\nDROP TABLE old_stuff;\n").unwrap();
    pgsafe().arg("--fix").arg(&f).assert().failure().code(1);
    let after = fs::read_to_string(&f).unwrap();
    assert!(
        after.contains("CREATE INDEX CONCURRENTLY i ON t (c);"),
        "fixable finding should be applied: {after}"
    );
    assert!(
        after.contains("DROP TABLE old_stuff;"),
        "unfixable statement must be left unchanged: {after}"
    );
}

#[test]
fn fix_summary_reports_counts() {
    // Same partial fixture: 2 fixes apply, 2 findings are unfixable (drop-table +
    // the second statement's require-timeout, which carries no fix). The stderr
    // summary locks both the count and the unfixable suffix text.
    let dir = tempdir().unwrap();
    let f = dir.path().join("partial.sql");
    fs::write(&f, "CREATE INDEX i ON t (c);\nDROP TABLE old_stuff;\n").unwrap();
    pgsafe()
        .arg("--fix")
        .arg(&f)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("fixed 2 findings"))
        .stderr(predicate::str::contains("unfixable"));
}

#[test]
fn fix_composes_multiple_fixes_in_one_file() {
    // Two CREATE INDEX statements: both gain CONCURRENTLY in a single pass, and
    // require-timeout is satisfied by one prologue -> clean afterwards (exit 0).
    let dir = tempdir().unwrap();
    let f = dir.path().join("multi.sql");
    fs::write(&f, "CREATE INDEX i ON a (c);\nCREATE INDEX j ON b (c);\n").unwrap();
    pgsafe().arg("--fix").arg(&f).assert().success();
    let after = fs::read_to_string(&f).unwrap();
    assert!(
        after.contains("CREATE INDEX CONCURRENTLY i ON a (c);"),
        "first index not fixed: {after}"
    );
    assert!(
        after.contains("CREATE INDEX CONCURRENTLY j ON b (c);"),
        "second index not fixed: {after}"
    );
}

#[test]
fn diff_exit_zero_on_clean_file() {
    // No findings -> --diff exits 0 with empty stdout.
    let dir = tempdir().unwrap();
    let f = dir.path().join("clean.sql");
    fs::write(&f, "SELECT 1;\n").unwrap();
    pgsafe()
        .arg("--diff")
        .arg(&f)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn diff_unfixable_only_not_silent() {
    // A rename is a gating finding with no automatic fix: --diff writes an empty
    // diff (stdout) but must not exit nonzero silently — it explains on stderr.
    let dir = tempdir().unwrap();
    let f = dir.path().join("rename.sql");
    fs::write(&f, "ALTER TABLE old_name RENAME TO new_name;\n").unwrap();
    pgsafe()
        .arg("--diff")
        .arg(&f)
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("have no automatic fix"));
}

#[test]
fn diff_matches_fix_output() {
    // Round-trip: for a fixture where every line is touched, the `+`-prefixed diff
    // lines (concatenated) must reconstruct exactly what `--fix` writes.
    let dir = tempdir().unwrap();
    let df = dir.path().join("diff.sql");
    let ff = dir.path().join("fix.sql");
    let src = "CREATE INDEX i ON a (c);\nCREATE INDEX j ON b (c);\n";
    fs::write(&df, src).unwrap();
    fs::write(&ff, src).unwrap();

    let diff_out = pgsafe().arg("--diff").arg(&df).assert();
    let diff_stdout = String::from_utf8(diff_out.get_output().stdout.clone()).unwrap();
    let plus_lines: String = diff_stdout
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .map(|l| format!("{}\n", &l[1..]))
        .collect();

    let _ = pgsafe().arg("--fix").arg(&ff).assert();
    let fixed = fs::read_to_string(&ff).unwrap();

    assert_eq!(
        plus_lines, fixed,
        "diff `+` lines must reconstruct the --fix output"
    );
}

#[cfg(unix)]
#[test]
fn fix_write_error_exits_2() {
    use std::os::unix::fs::PermissionsExt;
    // A fixable file that can't be written (read-only) surfaces the IO error and
    // exits 2 with the path in stderr — never a swallowed failure.
    let dir = tempdir().unwrap();
    let f = dir.path().join("ro.sql");
    fs::write(&f, "CREATE INDEX i ON t (c);\n").unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o444)).unwrap();
    pgsafe()
        .arg("--fix")
        .arg(&f)
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(f.to_str().unwrap()));
}
