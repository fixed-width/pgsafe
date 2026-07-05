//! SARIF 2.1.0 output (`--format sarif`) for GitHub code-scanning ingestion.
//! Hand-rolled `serde::Serialize` structs for the subset pgsafe emits; serialized
//! with `serde_json`, like the JSON envelope in `output.rs`.

use crate::{FileReport, Severity};

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
    suppressions: Vec<Suppression>,
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
struct Suppression {
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
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
                vec![Suppression { kind: "inSource" }]
            } else {
                Vec::new()
            };
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
        assert!(r["level"] == "error" || r["level"] == "warning");
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
        assert!(
            rule["defaultConfiguration"]["level"] == "error"
                || rule["defaultConfiguration"]["level"] == "warning"
        );
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
}
