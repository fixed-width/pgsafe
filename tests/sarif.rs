use assert_cmd::Command;

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
