use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn flags_non_concurrent_index_from_stdin() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("CREATE INDEX i ON t (x);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("non-concurrent-index"));
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
