use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn run_in(dir: &std::path::Path, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .current_dir(dir)
        .args(args)
        .assert()
}

#[test]
fn since_lints_only_files_after_the_cutoff() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("0001_legacy.sql"), "DROP TABLE a;\n").unwrap();
    fs::write(dir.path().join("0002_cut.sql"), "DROP TABLE b;\n").unwrap();
    fs::write(
        dir.path().join("0003_new.sql"),
        "CREATE INDEX i ON t (x);\n",
    )
    .unwrap();
    run_in(
        dir.path(),
        &[
            "--since",
            "0002_cut.sql",
            "0001_legacy.sql",
            "0002_cut.sql",
            "0003_new.sql",
        ],
    )
    // 0001/0002 (<= cutoff) excluded → no drop-table; 0003 (> cutoff) linted → add-index fires.
    .stdout(
        predicate::str::contains("drop-table")
            .not()
            .and(predicate::str::contains("add-index-non-concurrent")),
    );
}

#[test]
fn config_since_is_honored_and_cli_overrides_it() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join(".pgsafe.toml"),
        "since = \"0002_cut.sql\"\n",
    )
    .unwrap();
    fs::write(dir.path().join("0001_legacy.sql"), "DROP TABLE a;\n").unwrap();
    fs::write(dir.path().join("0002_cut.sql"), "DROP TABLE b;\n").unwrap();
    fs::write(dir.path().join("0003_new.sql"), "DROP TABLE c;\n").unwrap();
    // config since=0002 → only 0003 is linted.
    run_in(
        dir.path(),
        &["0001_legacy.sql", "0002_cut.sql", "0003_new.sql"],
    )
    .stdout(
        predicate::str::contains("0003_new.sql")
            .and(predicate::str::contains("0001_legacy.sql").not()),
    );
    // CLI --since 0001 overrides config → 0002 and 0003 linted (not 0001).
    run_in(
        dir.path(),
        &[
            "--since",
            "0001_legacy.sql",
            "0001_legacy.sql",
            "0002_cut.sql",
            "0003_new.sql",
        ],
    )
    .stdout(
        predicate::str::contains("0002_cut.sql")
            .and(predicate::str::contains("0003_new.sql"))
            .and(predicate::str::contains("0001_legacy.sql").not()),
    );
}

#[test]
fn since_composes_with_paths_scoping() {
    // `--since` selects files after the cutoff; the config's `paths` globs then scope
    // them. A post-cutoff file outside `paths` is still dropped — proving the scope
    // filter runs uniformly after selection, not only over bare positional inputs.
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join(".pgsafe.toml"),
        "paths = [\"migrations/**\"]\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("migrations")).unwrap();
    fs::create_dir_all(dir.path().join("queries")).unwrap();
    // Both sort after the cutoff and both would flag add-index; only the in-scope one lints.
    fs::write(
        dir.path().join("migrations/0003_new.sql"),
        "CREATE INDEX i ON t (x);\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("queries/0003_report.sql"),
        "CREATE INDEX j ON t (y);\n",
    )
    .unwrap();
    run_in(
        dir.path(),
        &[
            "--since",
            "0002_cut.sql",
            "migrations/0003_new.sql",
            "queries/0003_report.sql",
        ],
    )
    .failure()
    .code(1)
    .stdout(
        predicate::str::contains("migrations/0003_new.sql")
            .and(predicate::str::contains("add-index-non-concurrent"))
            .and(predicate::str::contains("queries/0003_report.sql").not()),
    );
}

#[test]
fn since_and_git_diff_are_mutually_exclusive() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("m.sql"), "DROP TABLE a;\n").unwrap();
    run_in(
        dir.path(),
        &["--since", "0001.sql", "--git-diff", "HEAD", "m.sql"],
    )
    .failure()
    .code(2); // clap conflicts_with → usage error
}

#[test]
fn since_rejects_stdin() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("m.sql"), "DROP TABLE a;\n").unwrap();
    // Explicit --since with `-` in the paths.
    run_in(dir.path(), &["--since", "0001.sql", "m.sql", "-"])
        .failure()
        .code(2)
        .stderr(predicate::str::contains("stdin"));
    // A config `since` cutoff also rejects stdin, and the message mentions `since`.
    fs::write(dir.path().join(".pgsafe.toml"), "since = \"0001.sql\"\n").unwrap();
    run_in(dir.path(), &["m.sql", "-"])
        .failure()
        .code(2)
        .stderr(predicate::str::contains("since"));
}
