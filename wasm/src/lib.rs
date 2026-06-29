//! Spike entry point: lint an input string and return the same JSON envelope
//! as `pgsafe --format json`. Shared by the WASI command (`main.rs`) and tests.

/// Lint `sql` with default options and return the pgsafe JSON envelope.
#[must_use]
pub fn run(sql: &str) -> String {
    let report = pgsafe::lint_input("playground.sql", sql, &pgsafe::LintOptions::default());
    match pgsafe::render_json(&[report]) {
        Ok(json) => json,
        Err(e) => format!(r#"{{"error":"render: {e}"}}"#),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn flags_non_concurrent_index() {
        let out = super::run("CREATE INDEX idx ON t (col);");
        assert!(
            out.contains("add-index-non-concurrent"),
            "expected rule id in output, got: {out}"
        );
    }
}
