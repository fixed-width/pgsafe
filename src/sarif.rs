//! SARIF 2.1.0 output (`--format sarif`) for GitHub code-scanning ingestion.
//! Hand-rolled `serde::Serialize` structs for the subset pgsafe emits; serialized
//! with `serde_json`, like the JSON envelope in `output.rs`.

use crate::{FileReport, Severity};
use std::collections::{BTreeMap, HashMap};

const SCHEMA_URL: &str = "https://json.schemastore.org/sarif-2.1.0.json";
const INFORMATION_URI: &str = "https://pgsafe.fixedwidth.tech";
const RULE_HELP_BASE: &str = "https://pgsafe.fixedwidth.tech/rules/";

#[derive(serde::Serialize)]
struct SarifLog {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: Vec<Run>,
}

#[derive(serde::Serialize)]
struct Run {
    tool: Tool,
    results: Vec<SarifResult>,
    invocations: Vec<Invocation>,
}

#[derive(serde::Serialize)]
struct Tool {
    driver: Driver,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Driver {
    name: &'static str,
    version: &'static str,
    information_uri: &'static str,
    rules: Vec<ReportingDescriptor>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportingDescriptor {
    id: String,
    help_uri: String,
    default_configuration: Configuration,
}

#[derive(serde::Serialize)]
struct Configuration {
    level: &'static str,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifResult {
    rule_id: String,
    rule_index: usize,
    level: &'static str,
    message: Message,
    locations: Vec<SarifLocation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    suppressions: Vec<SarifSuppression>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    partial_fingerprints: BTreeMap<String, String>,
}

#[derive(serde::Serialize)]
struct Message {
    text: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifLocation {
    physical_location: PhysicalLocation,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PhysicalLocation {
    artifact_location: ArtifactLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<Region>,
}

#[derive(serde::Serialize)]
struct ArtifactLocation {
    uri: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Region {
    start_line: u32,
    start_column: u32,
}

#[derive(serde::Serialize)]
struct SarifSuppression {
    kind: &'static str,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Invocation {
    execution_successful: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_execution_notifications: Vec<Notification>,
}

#[derive(serde::Serialize)]
struct Notification {
    level: &'static str,
    message: Message,
    locations: Vec<SarifLocation>,
}

/// SARIF level string for a severity.
fn level(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    }
}

/// One `location` pointing at `uri`, optionally with a 1-based line/column region.
fn location(uri: &str, region: Option<Region>) -> SarifLocation {
    SarifLocation {
        physical_location: PhysicalLocation {
            artifact_location: ArtifactLocation {
                uri: uri.to_string(),
            },
            region,
        },
    }
}

/// FNV-1a 64-bit hash of `input` as zero-padded 16-char lowercase hex. A fixed
/// algorithm (offset basis / prime below), so its output is stable across Rust
/// versions — required for a code-scanning fingerprint compared across uploads
/// over time. (`std::hash::DefaultHasher` is explicitly not stable across versions.)
fn fnv1a_hex(input: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in input.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Render `reports` as a SARIF 2.1.0 log. Findings (including suppressed ones)
/// become `results`; a file that failed to parse becomes a tool-execution
/// notification and flips `executionSuccessful` to false.
///
/// # Errors
/// Returns a message if serialization fails.
pub fn render_sarif(reports: &[FileReport]) -> Result<String, String> {
    let mut rules: Vec<ReportingDescriptor> = Vec::new();
    let mut results: Vec<SarifResult> = Vec::new();
    let mut notifications: Vec<Notification> = Vec::new();
    let mut execution_successful = true;
    let mut fp_ordinals: HashMap<(String, String, String), u32> = HashMap::new();

    for r in reports {
        if let Some(err) = &r.error {
            execution_successful = false;
            notifications.push(Notification {
                level: "error",
                message: Message { text: err.clone() },
                locations: vec![location(&r.name, None)],
            });
            continue;
        }
        for f in &r.findings {
            // Distinct rule_ids, first-seen order (rule count is tiny → linear find).
            let rule_index = match rules.iter().position(|rd| rd.id == f.rule_id) {
                Some(i) => i,
                None => {
                    rules.push(ReportingDescriptor {
                        id: f.rule_id.clone(),
                        help_uri: format!("{RULE_HELP_BASE}{}", f.rule_id),
                        default_configuration: Configuration {
                            level: level(f.severity),
                        },
                    });
                    rules.len() - 1
                }
            };
            let suppressions = if f.is_suppressed() {
                vec![SarifSuppression { kind: "inSource" }]
            } else {
                Vec::new()
            };
            // Line-independent fingerprint; the per-(file, rule, snippet) ordinal keeps
            // byte-identical statements distinct without depending on line numbers.
            let ordinal = {
                let key = (r.name.clone(), f.rule_id.clone(), f.snippet.clone());
                let n = fp_ordinals.entry(key).or_insert(0);
                let cur = *n;
                *n += 1;
                cur
            };
            let fingerprint = fnv1a_hex(&format!("{}\n{}\n{}", f.rule_id, f.snippet, ordinal));
            let mut partial_fingerprints = BTreeMap::new();
            partial_fingerprints.insert("pgsafe/v1".to_string(), fingerprint);
            results.push(SarifResult {
                rule_id: f.rule_id.clone(),
                rule_index,
                level: level(f.severity),
                message: Message {
                    text: f.message.clone(),
                },
                locations: vec![location(
                    &r.name,
                    Some(Region {
                        start_line: f.location.line,
                        start_column: f.location.column,
                    }),
                )],
                suppressions,
                partial_fingerprints,
            });
        }
    }

    let log = SarifLog {
        schema: SCHEMA_URL,
        version: "2.1.0",
        runs: vec![Run {
            tool: Tool {
                driver: Driver {
                    name: "pgsafe",
                    version: env!("CARGO_PKG_VERSION"),
                    information_uri: INFORMATION_URI,
                    rules,
                },
            },
            results,
            invocations: vec![Invocation {
                execution_successful,
                tool_execution_notifications: notifications,
            }],
        }],
    };
    serde_json::to_string_pretty(&log).map_err(|e| format!("failed to serialize SARIF output: {e}"))
}

#[cfg(test)]
mod tests {
    use super::render_sarif;
    use crate::{lint_input, FileReport, LintOptions};

    fn value(reports: &[FileReport]) -> serde_json::Value {
        serde_json::from_str(&render_sarif(reports).unwrap()).unwrap()
    }

    #[test]
    fn emits_sarif_2_1_0_envelope() {
        let reports = vec![lint_input(
            "m.sql",
            "CREATE INDEX i ON t (x);",
            &LintOptions::default(),
        )];
        let v = value(&reports);
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(
            v["$schema"],
            "https://json.schemastore.org/sarif-2.1.0.json"
        );
        let driver = &v["runs"][0]["tool"]["driver"];
        assert_eq!(driver["name"], "pgsafe");
        assert!(!driver["version"].as_str().unwrap().is_empty());
        assert_eq!(driver["informationUri"], "https://pgsafe.fixedwidth.tech");
    }

    #[test]
    fn finding_becomes_a_result_with_rule_and_location() {
        let reports = vec![lint_input(
            "m.sql",
            "CREATE INDEX i ON t (x);",
            &LintOptions::default(),
        )];
        let v = value(&reports);
        let results = v["runs"][0]["results"].as_array().unwrap();
        let r = results
            .iter()
            .find(|r| r["ruleId"] == "add-index-non-concurrent")
            .unwrap();
        // add-index-non-concurrent is unconditionally Error.
        assert_eq!(r["level"], "error");
        assert!(!r["message"]["text"].as_str().unwrap().is_empty());
        let loc = &r["locations"][0]["physicalLocation"];
        assert_eq!(loc["artifactLocation"]["uri"], "m.sql");
        assert!(loc["region"]["startLine"].as_u64().unwrap() >= 1);
        let idx = usize::try_from(r["ruleIndex"].as_u64().unwrap()).unwrap();
        let rule = &v["runs"][0]["tool"]["driver"]["rules"][idx];
        assert_eq!(rule["id"], "add-index-non-concurrent");
        assert_eq!(
            rule["helpUri"],
            "https://pgsafe.fixedwidth.tech/rules/add-index-non-concurrent"
        );
        assert_eq!(rule["defaultConfiguration"]["level"], "error");
    }

    #[test]
    fn suppressed_finding_carries_suppressions() {
        let sql = "-- pgsafe:ignore add-index-non-concurrent  in a maintenance window\nCREATE INDEX i ON t (x);";
        let reports = vec![lint_input("m.sql", sql, &LintOptions::default())];
        let v = value(&reports);
        let r = v["runs"][0]["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["ruleId"] == "add-index-non-concurrent")
            .unwrap()
            .clone();
        assert_eq!(r["suppressions"][0]["kind"], "inSource");
    }

    #[test]
    fn unsuppressed_finding_omits_suppressions() {
        let reports = vec![lint_input(
            "m.sql",
            "CREATE INDEX i ON t (x);",
            &LintOptions::default(),
        )];
        let v = value(&reports);
        let r = &v["runs"][0]["results"][0];
        assert!(r.get("suppressions").is_none());
    }

    #[test]
    fn parse_error_becomes_a_notification_not_a_result() {
        let reports = vec![lint_input(
            "bad.sql",
            "ALTER TABLE;",
            &LintOptions::default(),
        )];
        let v = value(&reports);
        assert_eq!(v["runs"][0]["results"].as_array().unwrap().len(), 0);
        let inv = &v["runs"][0]["invocations"][0];
        assert_eq!(inv["executionSuccessful"], false);
        let note = &inv["toolExecutionNotifications"][0];
        assert_eq!(note["level"], "error");
        assert!(note["message"]["text"]
            .as_str()
            .unwrap()
            .contains("parse error"));
        assert_eq!(
            note["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "bad.sql"
        );
    }

    #[test]
    fn clean_input_is_valid_empty_sarif() {
        let reports = vec![lint_input("ok.sql", "SELECT 1;", &LintOptions::default())];
        let v = value(&reports);
        assert_eq!(v["runs"][0]["results"].as_array().unwrap().len(), 0);
        assert_eq!(v["runs"][0]["invocations"][0]["executionSuccessful"], true);
    }

    #[test]
    fn a_rule_firing_twice_dedups_into_one_rules_entry() {
        let reports = vec![lint_input(
            "m.sql",
            "CREATE INDEX i ON a (x);\nCREATE INDEX j ON b (y);",
            &LintOptions::default(),
        )];
        let v = value(&reports);
        let rules = v["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap();
        // add-index-non-concurrent appears exactly once in rules[].
        let rule_count = rules
            .iter()
            .filter(|rd| rd["id"] == "add-index-non-concurrent")
            .count();
        assert_eq!(rule_count, 1);
        // both results for that rule share one ruleIndex, which resolves back to it.
        let results = v["runs"][0]["results"].as_array().unwrap();
        let idxs: Vec<u64> = results
            .iter()
            .filter(|r| r["ruleId"] == "add-index-non-concurrent")
            .map(|r| r["ruleIndex"].as_u64().unwrap())
            .collect();
        assert_eq!(idxs.len(), 2);
        assert_eq!(idxs[0], idxs[1]);
        let idx = usize::try_from(idxs[0]).unwrap();
        assert_eq!(rules[idx]["id"], "add-index-non-concurrent");
    }

    #[test]
    fn empty_reports_is_valid_sarif() {
        let v: serde_json::Value = serde_json::from_str(&render_sarif(&[]).unwrap()).unwrap();
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(v["runs"][0]["results"].as_array().unwrap().len(), 0);
        assert_eq!(v["runs"][0]["invocations"][0]["executionSuccessful"], true);
    }

    #[test]
    fn a_rule_firing_in_two_files_shares_one_rule_entry() {
        let reports = vec![
            lint_input("a.sql", "CREATE INDEX i ON a (x);", &LintOptions::default()),
            lint_input("b.sql", "CREATE INDEX j ON b (y);", &LintOptions::default()),
        ];
        let v = value(&reports);
        // Exactly one rules[] entry for the shared rule.
        let rules = v["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap();
        assert_eq!(
            rules
                .iter()
                .filter(|rd| rd["id"] == "add-index-non-concurrent")
                .count(),
            1
        );
        // Both results reference that same rule index and carry their own file's uri.
        let results = v["runs"][0]["results"].as_array().unwrap();
        let hits: Vec<&serde_json::Value> = results
            .iter()
            .filter(|r| r["ruleId"] == "add-index-non-concurrent")
            .collect();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0]["ruleIndex"], hits[1]["ruleIndex"]);
        let uri = |r: &serde_json::Value| {
            r["locations"][0]["physicalLocation"]["artifactLocation"]["uri"].clone()
        };
        assert_eq!(uri(hits[0]), "a.sql");
        assert_eq!(uri(hits[1]), "b.sql");
    }

    #[test]
    fn mixed_parse_error_and_findings_coexist() {
        let reports = vec![
            lint_input(
                "ok.sql",
                "CREATE INDEX i ON t (x);",
                &LintOptions::default(),
            ),
            lint_input("bad.sql", "ALTER TABLE;", &LintOptions::default()),
        ];
        let v = value(&reports);
        // ok.sql still produces results.
        assert!(!v["runs"][0]["results"].as_array().unwrap().is_empty());
        // bad.sql becomes a notification and flips the run to unsuccessful.
        let inv = &v["runs"][0]["invocations"][0];
        assert_eq!(inv["executionSuccessful"], false);
        assert!(inv["toolExecutionNotifications"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |n| n["locations"][0]["physicalLocation"]["artifactLocation"]["uri"] == "bad.sql"
            ));
    }

    #[test]
    fn severity_levels_are_concrete() {
        // VACUUM FULL fires vacuum-full-cluster (error) + require-timeout (warning).
        let reports = vec![lint_input(
            "m.sql",
            "VACUUM FULL t;",
            &LintOptions::default(),
        )];
        let v = value(&reports);
        let results = v["runs"][0]["results"].as_array().unwrap();
        let rules = v["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap();
        // Both concrete levels appear, and each result's ruleIndex resolves to a rules[]
        // entry whose defaultConfiguration.level matches that result's level.
        for want in ["error", "warning"] {
            let r = results
                .iter()
                .find(|r| r["level"] == want)
                .unwrap_or_else(|| panic!("expected a result with level {want}"));
            let idx = usize::try_from(r["ruleIndex"].as_u64().unwrap()).unwrap();
            assert_eq!(rules[idx]["defaultConfiguration"]["level"], want);
        }
    }

    #[test]
    fn fnv1a_hex_is_deterministic_golden() {
        assert_eq!(super::fnv1a_hex("abc"), "e71fa2190541574b");
    }

    #[test]
    fn results_carry_a_partial_fingerprint() {
        let reports = vec![lint_input(
            "m.sql",
            "CREATE INDEX i ON t (x);",
            &LintOptions::default(),
        )];
        let v = value(&reports);
        let fp = v["runs"][0]["results"][0]["partialFingerprints"]["pgsafe/v1"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn fingerprint_is_line_independent() {
        // The same statement, once at line 1 and once pushed down by a leading comment,
        // must produce the SAME fingerprint (only startLine differs).
        let find = |sql: &str| -> String {
            let reports = vec![lint_input("m.sql", sql, &LintOptions::default())];
            value(&reports)["runs"][0]["results"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["ruleId"] == "add-index-non-concurrent")
                .unwrap()["partialFingerprints"]["pgsafe/v1"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let at_top = find("CREATE INDEX i ON t (x);");
        let shifted = find("-- a comment\n\nCREATE INDEX i ON t (x);");
        assert_eq!(at_top, shifted);
    }

    #[test]
    fn duplicate_statements_get_distinct_fingerprints() {
        // Two byte-identical flagged statements in one file → ordinals 0 and 1 → different fps.
        let reports = vec![lint_input(
            "m.sql",
            "CREATE INDEX i ON t (x);\nCREATE INDEX i ON t (x);",
            &LintOptions::default(),
        )];
        let v = value(&reports);
        let fps: Vec<String> = v["runs"][0]["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["ruleId"] == "add-index-non-concurrent")
            .map(|r| {
                r["partialFingerprints"]["pgsafe/v1"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(fps.len(), 2);
        assert_ne!(fps[0], fps[1]);
    }

    #[test]
    fn different_rules_get_distinct_fingerprints() {
        let reports = vec![lint_input(
            "m.sql",
            "CREATE INDEX i ON t (x);\nDROP TABLE old;",
            &LintOptions::default(),
        )];
        let v = value(&reports);
        let results = v["runs"][0]["results"].as_array().unwrap();
        let fp = |rule: &str| -> String {
            results.iter().find(|r| r["ruleId"] == rule).unwrap()["partialFingerprints"]
                ["pgsafe/v1"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_ne!(fp("add-index-non-concurrent"), fp("drop-table"));
    }
}
