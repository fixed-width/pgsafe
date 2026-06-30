//! Playground entry point: lint a `{sql, inTransaction}` JSON request and return
//! the same JSON envelope as `pgsafe --format json`. Shared by the WASI command
//! (`main.rs`) and tests.

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    sql: String,
    #[serde(default)]
    in_transaction: bool,
}

/// Build an `{"error": <msg>}` envelope with correct JSON escaping.
fn error_json(msg: String) -> String {
    serde_json::json!({ "error": msg }).to_string()
}

/// Lint a JSON request string and return the pgsafe JSON envelope (or an
/// `{"error": ...}` object if the request itself can't be parsed).
#[must_use]
pub fn lint_json(request: &str) -> String {
    let req: Request = match serde_json::from_str(request) {
        Ok(r) => r,
        Err(e) => return error_json(format!("bad request: {e}")),
    };
    // LintOptions is #[non_exhaustive]: build via default, then set fields.
    let mut options = pgsafe::LintOptions::default();
    options.assume_in_transaction = req.in_transaction;
    let report = pgsafe::lint_input("playground.sql", &req.sql, &options);
    match pgsafe::render_json(&[report]) {
        Ok(json) => json,
        Err(e) => error_json(format!("render: {e}")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn flags_non_concurrent_index() {
        let out = super::lint_json(r#"{"sql":"CREATE INDEX idx ON t (col);"}"#);
        assert!(out.contains("add-index-non-concurrent"), "got: {out}");
    }

    #[test]
    fn in_transaction_flags_concurrently() {
        let req = r#"{"sql":"CREATE INDEX CONCURRENTLY i ON t (c);","inTransaction":true}"#;
        let out = super::lint_json(req);
        assert!(out.contains("concurrently-in-transaction"), "got: {out}");
    }

    #[test]
    fn bad_request_is_a_valid_json_error_envelope() {
        // A wrong-typed field yields a serde error whose text contains quotes;
        // the envelope must still be valid JSON (not hand-rolled string-bashing).
        for req in [
            r#"{}"#,                                // missing `sql`
            r#"{"sqll":"x"}"#,                      // unknown field (deny_unknown_fields)
            r#"{"sql":"x","inTransaction":"yes"}"#, // wrong type, quotes in error
            r#"not json"#,
        ] {
            let out = super::lint_json(req);
            let v: serde_json::Value = serde_json::from_str(&out)
                .unwrap_or_else(|_| panic!("invalid JSON for {req}: {out}"));
            assert!(
                v["error"].is_string(),
                "expected error envelope for {req}, got {out}"
            );
        }
    }
}
