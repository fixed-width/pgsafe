use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// A temp git repo with one committed base `.sql` file. The base commit is `HEAD`.
fn repo_with_base(base_name: &str, base_sql: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(dir.path().join(base_name), base_sql).unwrap();
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "base"]);
    dir
}

fn git(dir: &Path, args: &[&str]) {
    let status = StdCommand::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn pgsafe(dir: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .current_dir(dir)
        .args(args)
        .assert()
}

#[test]
fn lints_a_new_changed_sql_file() {
    let dir = repo_with_base("0001_base.sql", "CREATE TABLE t (id bigint);\n");
    fs::write(dir.path().join("0002_new.sql"), "DROP TABLE t;\n").unwrap();
    pgsafe(dir.path(), &["--git-diff", "HEAD"])
        .failure()
        .code(1) // drop-table (warning) gates under the default fail_on
        .stdout(predicate::str::contains("drop-table"));
}

#[test]
fn does_not_lint_unchanged_files() {
    // The base file WOULD flag drop-table, but it is unchanged → not selected.
    let dir = repo_with_base("0001_base.sql", "DROP TABLE legacy;\n");
    fs::write(
        dir.path().join("0002_new.sql"),
        "CREATE TABLE ok (id bigint);\n",
    )
    .unwrap();
    pgsafe(dir.path(), &["--git-diff", "HEAD"])
        .success()
        .stdout(predicate::str::contains("drop-table").not());
}

#[test]
fn no_changed_sql_exits_zero() {
    let dir = repo_with_base("0001_base.sql", "CREATE TABLE t (id bigint);\n");
    // Nothing changed since HEAD.
    pgsafe(dir.path(), &["--git-diff", "HEAD"]).success();
}

#[test]
fn changed_non_sql_is_ignored() {
    let dir = repo_with_base("0001_base.sql", "CREATE TABLE t (id bigint);\n");
    fs::write(dir.path().join("notes.md"), "# not sql\n").unwrap();
    pgsafe(dir.path(), &["--git-diff", "HEAD"]).success();
}

#[test]
fn scope_path_narrows_selection() {
    let dir = repo_with_base("0001_base.sql", "CREATE TABLE t (id bigint);\n");
    fs::create_dir_all(dir.path().join("db")).unwrap();
    fs::create_dir_all(dir.path().join("other")).unwrap();
    fs::write(dir.path().join("db/0002.sql"), "DROP TABLE a;\n").unwrap();
    fs::write(dir.path().join("other/0003.sql"), "DROP TABLE b;\n").unwrap();
    // Scope to db/ → only db/0002 is linted; other/0003 is not.
    pgsafe(dir.path(), &["--git-diff", "HEAD", "db"])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("0002").and(predicate::str::contains("0003").not()));
}

#[test]
fn composes_with_config_ignore() {
    // Config (committed in the base) disables drop-table; a new file's drop-table is suppressed,
    // even though the file is git-selected.
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(
        dir.path().join(".pgsafe.toml"),
        "[rules]\ndrop-table = false\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("0001_base.sql"),
        "CREATE TABLE t (id bigint);\n",
    )
    .unwrap();
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "base"]);
    fs::write(dir.path().join("0002_new.sql"), "DROP TABLE t;\n").unwrap();
    pgsafe(dir.path(), &["--git-diff", "HEAD"])
        .stdout(predicate::str::contains("drop-table").not());
}

#[test]
fn bad_ref_exits_two_with_a_fetch_hint() {
    let dir = repo_with_base("0001_base.sql", "CREATE TABLE t (id bigint);\n");
    pgsafe(dir.path(), &["--git-diff", "no-such-ref"])
        .failure()
        .code(2)
        .stderr(predicate::str::contains("fetch").and(predicate::str::contains("not required")));
}

#[test]
fn outside_a_repo_exits_two() {
    let dir = tempfile::tempdir().unwrap(); // no `git init`
    fs::write(dir.path().join("m.sql"), "DROP TABLE t;\n").unwrap();
    pgsafe(dir.path(), &["--git-diff", "HEAD"])
        .failure()
        .code(2);
}

#[test]
fn stdin_with_git_diff_is_rejected() {
    let dir = repo_with_base("0001_base.sql", "CREATE TABLE t (id bigint);\n");
    pgsafe(dir.path(), &["--git-diff", "HEAD", "-"])
        .failure()
        .code(2)
        .stderr(predicate::str::contains("stdin"));
}

#[test]
fn works_from_a_subdirectory_with_untracked_file() {
    let dir = repo_with_base("0001_base.sql", "CREATE TABLE t (id bigint);\n");
    fs::create_dir_all(dir.path().join("db/migrate")).unwrap();
    // An untracked new migration in a nested dir.
    fs::write(dir.path().join("db/migrate/0002.sql"), "DROP TABLE t;\n").unwrap();
    // Invoke pgsafe FROM the nested dir; the untracked file must still be found + linted.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .current_dir(dir.path().join("db/migrate"))
        .args(["--git-diff", "HEAD"])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("drop-table"));
}

#[test]
fn finds_a_changed_file_with_a_space_in_the_name() {
    // git quotes names with spaces/non-ASCII unless `-z` is used; without the fix this
    // file's `.sql` extension is hidden behind a trailing quote and it is dropped.
    let dir = repo_with_base("0001_base.sql", "CREATE TABLE t (id bigint);\n");
    fs::write(dir.path().join("0002 new.sql"), "DROP TABLE t;\n").unwrap();
    pgsafe(dir.path(), &["--git-diff", "HEAD"])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("drop-table"));
}
