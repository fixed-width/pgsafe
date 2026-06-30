use assert_cmd::Command;

#[test]
fn list_rules_human_lists_known_ids() {
    let out = Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--list-rules")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let ids: Vec<&str> = stdout.lines().collect();
    // Exact, ordered match — catches a rename/reorder that preserves the count.
    assert_eq!(ids, pgsafe::list_rule_ids(), "got: {stdout}");
}

#[test]
fn list_rules_json_is_a_versioned_envelope() {
    let out = Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--list-rules", "--format", "json"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["schema_version"], 1);
    let rules: Vec<&str> = v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    // JSON and the human list must be the same ordered set as the catalog.
    assert_eq!(rules, pgsafe::list_rule_ids());
}
