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
    assert!(ids.contains(&"add-index-non-concurrent"), "got: {stdout}");
    assert_eq!(ids.len(), pgsafe::list_rule_ids().len());
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
    let rules = v["rules"].as_array().unwrap();
    assert_eq!(rules.len(), pgsafe::list_rule_ids().len());
    assert!(rules.iter().any(|r| r == "add-index-non-concurrent"));
}
