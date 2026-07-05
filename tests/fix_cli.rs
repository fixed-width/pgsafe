use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::process::Command as StdCommand;
use tempfile::tempdir;

fn pgsafe() -> Command {
    Command::cargo_bin("pgsafe").unwrap()
}

fn git_available() -> bool {
    StdCommand::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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

#[test]
fn diff_output_applies_cleanly_with_git_apply() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let dir = tempdir().unwrap();
    assert!(StdCommand::new("git")
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap()
        .status
        .success());
    let rel = "m.sql";
    let original = "CREATE INDEX i ON t (x);\nSELECT 1;\nDROP TABLE stale;\n";
    fs::write(dir.path().join(rel), original).unwrap();

    // Capture --diff output as a patch.
    let diff = pgsafe()
        .arg("--diff")
        .arg(rel)
        .current_dir(dir.path())
        .assert();
    let patch = String::from_utf8(diff.get_output().stdout.clone()).unwrap();
    fs::write(dir.path().join("fix.patch"), &patch).unwrap();

    // git apply --check must accept it, and applying it must equal what --fix writes.
    let check = StdCommand::new("git")
        .args(["apply", "--check", "fix.patch"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "git apply --check failed:\n{}\n---patch---\n{patch}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(StdCommand::new("git")
        .args(["apply", "fix.patch"])
        .current_dir(dir.path())
        .output()
        .unwrap()
        .status
        .success());
    let applied_via_diff = fs::read_to_string(dir.path().join(rel)).unwrap();

    // Compare against --fix on a fresh copy.
    let dir2 = tempdir().unwrap();
    fs::write(dir2.path().join(rel), original).unwrap();
    let _ = pgsafe()
        .arg("--fix")
        .arg(rel)
        .current_dir(dir2.path())
        .assert();
    let applied_via_fix = fs::read_to_string(dir2.path().join(rel)).unwrap();

    assert_eq!(applied_via_diff, applied_via_fix);
}

#[test]
fn diff_cross_hunk_delta_applies_cleanly_with_git_apply() {
    // Two fixable CREATE INDEX statements separated by MORE than 2*CONTEXT (=6)
    // unchanged lines, so they do NOT coalesce -> two hunks. The first hunk also
    // gains the require-timeout prologue (a line added at the top), making the
    // running `new_delta` nonzero, which shifts the SECOND hunk's `new_start`. The
    // git round-trip is what validates that offset arithmetic (git apply rejects
    // wrong offsets); the two-hunk assertion stops the test silently degrading to
    // one hunk.
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let dir = tempdir().unwrap();
    assert!(StdCommand::new("git")
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap()
        .status
        .success());
    let rel = "m.sql";
    let original = "CREATE INDEX i ON a (x);\n\
         SELECT 1;\nSELECT 2;\nSELECT 3;\nSELECT 4;\nSELECT 5;\nSELECT 6;\nSELECT 7;\n\
         CREATE INDEX j ON b (y);\n";
    fs::write(dir.path().join(rel), original).unwrap();

    let diff = pgsafe()
        .arg("--diff")
        .arg(rel)
        .current_dir(dir.path())
        .assert();
    let patch = String::from_utf8(diff.get_output().stdout.clone()).unwrap();
    // Exactly two hunks (the 7-line gap exceeds 2*CONTEXT, so no coalescing).
    assert_eq!(
        patch.lines().filter(|l| l.starts_with("@@ ")).count(),
        2,
        "expected two non-coalesced hunks:\n{patch}"
    );
    fs::write(dir.path().join("fix.patch"), &patch).unwrap();

    let check = StdCommand::new("git")
        .args(["apply", "--check", "fix.patch"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "git apply --check failed:\n{}\n---patch---\n{patch}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(StdCommand::new("git")
        .args(["apply", "fix.patch"])
        .current_dir(dir.path())
        .output()
        .unwrap()
        .status
        .success());
    let applied_via_diff = fs::read_to_string(dir.path().join(rel)).unwrap();

    let dir2 = tempdir().unwrap();
    fs::write(dir2.path().join(rel), original).unwrap();
    let _ = pgsafe()
        .arg("--fix")
        .arg(rel)
        .current_dir(dir2.path())
        .assert();
    let applied_via_fix = fs::read_to_string(dir2.path().join(rel)).unwrap();

    assert_eq!(applied_via_diff, applied_via_fix);
}

#[test]
fn diff_reports_unfixable_in_mixed_case() {
    // A fixable finding (CREATE INDEX → CONCURRENTLY) plus an unfixable gating one
    // (DROP TABLE): the non-empty diff still surfaces the residual unfixable finding.
    pgsafe()
        .arg("--diff")
        .write_stdin("CREATE INDEX i ON t (x);\nDROP TABLE stale;\n")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("+CREATE INDEX CONCURRENTLY"))
        .stderr(predicate::str::contains("have no automatic fix"));
}

#[test]
fn diff_skipped_overlapping_not_silent() {
    // Two DISTINCT fixable findings target the same type token: the built-in
    // `prefer-bigint-primary-key` and a `[forbidden-types]` policy on `integer`, both
    // `ReplaceTokenAt` the `integer` span. `apply_all` accepts one and SKIPS the other
    // for overlap (`skipped_overlapping = 1`). Both findings gate (warning is the
    // default fail-on), so `--diff` exits 1 — but the skipped fixable finding is in
    // neither the diff nor the old `unfixable`-only note. This asserts the residual
    // note now mentions the skipped/overlapping finding rather than exiting silently.
    // (`--fix` on the same input reports "(1 skipped-overlapping)", confirming the skip.)
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join(".pgsafe.toml"),
        "[forbidden-types]\ninteger = \"bigint\"\n",
    )
    .unwrap();
    let f = dir.path().join("t.sql");
    fs::write(&f, "CREATE TABLE t (id integer PRIMARY KEY);\n").unwrap();
    pgsafe()
        .arg("--diff")
        .arg("t.sql")
        .current_dir(dir.path())
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("skipped"))
        .stderr(predicate::str::contains("overlapping"));
}

#[cfg(unix)]
#[test]
fn fix_preserves_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    // A restrictive mode (0640) must survive the atomic temp+rename — the rename
    // otherwise resets the replacement inode to the umask default, silently loosening it.
    let dir = tempdir().unwrap();
    let f = dir.path().join("m.sql");
    fs::write(&f, "CREATE INDEX i ON t (x);\n").unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o640)).unwrap();
    let _ = pgsafe().arg("--fix").arg(&f).assert();
    assert!(fs::read_to_string(&f).unwrap().contains("CONCURRENTLY"));
    let mode = fs::metadata(&f).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640, "mode must be preserved, got {mode:o}");
}

#[cfg(unix)]
#[test]
fn fix_writes_through_symlink() {
    use std::os::unix::fs::symlink;
    // Running --fix on a SYMLINK must rewrite the REAL file (resolving the link),
    // leaving the symlink a symlink — not replace it with a regular file.
    let dir = tempdir().unwrap();
    let real = dir.path().join("real.sql");
    let link = dir.path().join("link.sql");
    fs::write(&real, "CREATE INDEX i ON t (x);\n").unwrap();
    symlink(&real, &link).unwrap();
    let _ = pgsafe().arg("--fix").arg(&link).assert();
    // The link is still a symlink...
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "link must remain a symlink"
    );
    // ...and the real file received the fix.
    assert!(
        fs::read_to_string(&real).unwrap().contains("CONCURRENTLY"),
        "real file must be rewritten through the symlink"
    );
    // No stray temp left behind.
    let siblings: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(siblings.is_empty(), "temp file left behind: {siblings:?}");
}

#[cfg(unix)]
#[test]
fn fix_temp_create_failure_exits_2() {
    use std::os::unix::fs::PermissionsExt;
    // The file is writable (readonly check passes) but its DIRECTORY is not, so the
    // sibling-temp create fails: the IO error must surface as exit 2, not a swallowed
    // failure. Restore dir perms afterwards so the tempdir can clean itself up.
    let dir = tempdir().unwrap();
    let f = dir.path().join("m.sql");
    fs::write(&f, "CREATE INDEX i ON t (x);\n").unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let result = pgsafe().arg("--fix").arg(&f).assert();
    // Restore before any assertion can unwind, so cleanup always succeeds.
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    result.failure().code(2);
}

#[test]
fn diff_no_trailing_newline_applies_cleanly_with_git_apply() {
    // Locks the `\ No newline at end of file` marker placement against real git: a
    // fixture with NO trailing newline must still round-trip through `git apply` and
    // match `--fix`.
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let dir = tempdir().unwrap();
    assert!(StdCommand::new("git")
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap()
        .status
        .success());
    let rel = "m.sql";
    let original = "CREATE INDEX i ON t (x);"; // no trailing newline
    fs::write(dir.path().join(rel), original).unwrap();

    let diff = pgsafe()
        .arg("--diff")
        .arg(rel)
        .current_dir(dir.path())
        .assert();
    let patch = String::from_utf8(diff.get_output().stdout.clone()).unwrap();
    assert!(
        patch.contains("\\ No newline at end of file"),
        "patch must carry the no-newline marker:\n{patch}"
    );
    fs::write(dir.path().join("fix.patch"), &patch).unwrap();

    let check = StdCommand::new("git")
        .args(["apply", "--check", "fix.patch"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "git apply --check failed:\n{}\n---patch---\n{patch}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(StdCommand::new("git")
        .args(["apply", "fix.patch"])
        .current_dir(dir.path())
        .output()
        .unwrap()
        .status
        .success());
    let applied_via_diff = fs::read_to_string(dir.path().join(rel)).unwrap();

    let dir2 = tempdir().unwrap();
    fs::write(dir2.path().join(rel), original).unwrap();
    let _ = pgsafe()
        .arg("--fix")
        .arg(rel)
        .current_dir(dir2.path())
        .assert();
    let applied_via_fix = fs::read_to_string(dir2.path().join(rel)).unwrap();

    assert_eq!(applied_via_diff, applied_via_fix);
}

#[test]
fn fix_write_is_atomic_no_temp_left_behind() {
    let dir = tempdir().unwrap();
    let f = dir.path().join("m.sql");
    fs::write(&f, "CREATE INDEX i ON t (x);\n").unwrap();
    let _ = pgsafe().arg("--fix").arg(&f).assert();
    // Fixed content written, and no leftover *.tmp sibling.
    assert!(fs::read_to_string(&f).unwrap().contains("CONCURRENTLY"));
    let siblings: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(siblings.is_empty(), "temp file left behind: {siblings:?}");
}
