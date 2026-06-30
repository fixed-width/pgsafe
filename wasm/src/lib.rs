//! Playground entry point: lint a `{sql, inTransaction}` JSON request and return
//! the same JSON envelope as `pgsafe --format json`. Shared by the WASI command
//! (`main.rs`) and tests.

use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Request {
    sql: String,
    in_transaction: bool,
}

/// Lint a JSON request string and return the pgsafe JSON envelope (or an
/// `{"error": ...}` object if the request itself can't be parsed).
#[must_use]
pub fn lint_json(request: &str) -> String {
    let req: Request = match serde_json::from_str(request) {
        Ok(r) => r,
        Err(e) => return format!(r#"{{"error":"bad request: {e}"}}"#),
    };
    // LintOptions is #[non_exhaustive]: build via default, then set fields.
    let mut options = pgsafe::LintOptions::default();
    options.assume_in_transaction = req.in_transaction;
    let report = pgsafe::lint_input("playground.sql", &req.sql, &options);
    match pgsafe::render_json(&[report]) {
        Ok(json) => json,
        Err(e) => format!(r#"{{"error":"render: {e}"}}"#),
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
}
