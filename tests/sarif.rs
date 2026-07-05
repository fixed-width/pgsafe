use std::fs;

use assert_cmd::Command;
use tempfile::tempdir;

fn pgsafe() -> Command {
    Command::cargo_bin("pgsafe").unwrap()
}

#[test]
fn sarif_format_emits_parseable_sarif_and_gates() {
    let assert = pgsafe()
        .args(["--format", "sarif"])
        .write_stdin("CREATE INDEX i ON t (x);\n")
        .assert()
        .failure()
        .code(1);
    let out = &assert.get_output().stdout;
    let v: serde_json::Value = serde_json::from_slice(out).unwrap();
    assert_eq!(v["version"], "2.1.0");
    assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "pgsafe");
    assert!(v["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["ruleId"] == "add-index-non-concurrent"));
}

#[test]
fn sarif_clean_input_exits_zero() {
    pgsafe()
        .args(["--format", "sarif"])
        .write_stdin("SELECT 1;\n")
        .assert()
        .success();
}

#[test]
fn fix_conflicts_with_sarif_format() {
    pgsafe()
        .args(["--fix", "--format", "sarif"])
        .write_stdin("SELECT 1;")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn sarif_parse_error_exits_2_with_notification() {
    let assert = pgsafe()
        .args(["--format", "sarif"])
        .write_stdin("ALTER TABLE;")
        .assert()
        .failure()
        .code(2);
    let v: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let inv = &v["runs"][0]["invocations"][0];
    assert_eq!(inv["executionSuccessful"], false);
    assert!(!inv["toolExecutionNotifications"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn fix_conflicts_with_config_format_sarif() {
    // A config-file `format = "sarif"` must still block --fix (the guard checks the
    // resolved format, not just the --format flag).
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(".pgsafe.toml"), "format = \"sarif\"\n").unwrap();
    pgsafe()
        .current_dir(dir.path())
        .args(["--fix", "-"])
        .write_stdin("CREATE INDEX i ON t (x);")
        .assert()
        .failure()
        .code(2);
}
