use assert_cmd::Command;
use predicates::prelude::*;

// ── `lsp` subcommand vs. positional-path coexistence ────────────────────────
//
// `CommonArgs` has a positional `paths: Vec<String>`. Adding an optional
// `Command` subcommand alongside it risks clap treating `lsp` as just another
// positional path, or (worse) treating an ordinary filename as an attempted
// subcommand. These tests pin the coexistence.

#[cfg(feature = "lsp")]
#[test]
fn lsp_subcommand_is_recognized() {
    // `--help` for the subcommand exits 0 and mentions the language server.
    let mut cmd = Command::cargo_bin("pgsafe").unwrap();
    cmd.args(["lsp", "--help"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("language server"));
}

#[cfg(feature = "lsp")]
#[test]
fn lsp_subcommand_serves_over_real_stdio_and_exits_cleanly() {
    // Regression test for a real deadlock: `server::serve()` originally kept its
    // `Connection` (and thus its message `Sender`) alive across `io_threads.join()`,
    // so the background writer thread's channel never saw its last sender drop and
    // `join()` blocked forever. The in-memory `Connection::memory()` harness used by
    // tests/lsp_server.rs never exercises this real-stdio path, so only spawning the
    // actual binary with piped stdin/stdout — as this test does — can catch it. A
    // bounded `.timeout()` turns a regression back into a failing test instead of a
    // hung `cargo test`.
    fn lsp_msg(body: &str) -> String {
        format!("Content-Length: {}\r\n\r\n{body}", body.len())
    }
    let payload = format!(
        "{}{}{}{}",
        lsp_msg(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#),
        lsp_msg(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#),
        lsp_msg(r#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}"#),
        lsp_msg(r#"{"jsonrpc":"2.0","method":"exit","params":null}"#),
    );

    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("lsp")
        .timeout(std::time::Duration::from_secs(10))
        .write_stdin(payload)
        .assert()
        .success();
}

#[test]
fn positional_path_still_lints_after_subcommand_added() {
    // A plain filename must still be treated as a lint target, not an unknown
    // subcommand, whether or not this build has the `lsp` subcommand at all.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("subcommand_coexistence.sql");
    std::fs::write(&path, "CREATE INDEX i ON t (x);").unwrap();

    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg(path.to_str().unwrap())
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("add-index-non-concurrent"));
}

// ── existing tests ───────────────────────────────────────────────────────────

#[test]
fn flags_add_index_non_concurrent_from_stdin() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("CREATE INDEX i ON t (x);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("add-index-non-concurrent"));
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

// ── file input ───────────────────────────────────────────────────────────────

#[test]
fn file_input_flags_findings_and_prints_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("migration.sql");
    std::fs::write(&path, "CREATE INDEX i ON t (x);").unwrap();

    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg(path.to_str().unwrap())
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("add-index-non-concurrent"))
        .stdout(predicate::str::contains(path.to_str().unwrap()));
}

// ── file not found ───────────────────────────────────────────────────────────

#[test]
fn file_not_found_exits_2_with_filename_in_stderr() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("no_such_file.sql")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no_such_file.sql"));
}

// ── parse error on stderr ────────────────────────────────────────────────────

#[test]
fn parse_error_exits_2_with_parse_error_in_stderr() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("ALTER TABLE;")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parse error"));
}

// ── clean SQL produces empty stdout ─────────────────────────────────────────

#[test]
fn clean_sql_produces_empty_stdout() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("CREATE INDEX CONCURRENTLY i ON t (x);")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

// ── multiple findings ────────────────────────────────────────────────────────

#[test]
fn multiple_findings_all_appear_in_output() {
    let output = Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("CREATE INDEX a ON t (x); CREATE INDEX b ON t (y);")
        .assert()
        .failure()
        .code(1)
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let count = stdout.matches("add-index-non-concurrent").count();
    assert_eq!(count, 2, "expected two findings, got {count} in:\n{stdout}");
}

// ── JSON structure ────────────────────────────────────────────────────────────

#[test]
fn json_format_structure_is_correct() {
    let out = Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--format", "json"])
        .write_stdin("CREATE INDEX i ON t (x);")
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(1));

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be valid JSON");

    assert_eq!(v["schema_version"], 2);
    assert_eq!(v["files"][0]["file"], "<stdin>");

    let fnd = &v["files"][0]["findings"][0];
    assert_eq!(fnd["rule_id"], "add-index-non-concurrent");
    assert_eq!(fnd["severity"], "error");
    assert!(fnd["location"]["line"].is_number(), "line must be a number");
    assert!(
        fnd["location"]["column"].is_number(),
        "column must be a number"
    );
    assert!(
        fnd["snippet"].as_str().unwrap().contains("CREATE INDEX"),
        "snippet must contain CREATE INDEX"
    );
}

#[test]
fn json_format_clean_sql_exits_0_with_empty_findings() {
    let out = Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--format", "json"])
        .write_stdin("CREATE INDEX CONCURRENTLY i ON t (x);")
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0));

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be valid JSON");

    let findings = v["files"][0]["findings"].as_array().unwrap();
    assert!(
        findings.is_empty(),
        "expected no findings, got {findings:?}"
    );
}

// ── error severity rendering ─────────────────────────────────────────────────

#[test]
fn error_severity_renders_in_human_and_json() {
    // human: a statement header ("{name}:{line}:{col}  {snippet}") then a nested
    // "  {severity} [{rule_id}]" line per finding.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("VACUUM FULL t;")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("error [vacuum-full-cluster]"));
    // json
    let out = Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--format", "json"])
        .write_stdin("VACUUM FULL t;")
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["files"][0]["findings"][0]["severity"], "error");
}

// ── suppression ──────────────────────────────────────────────────────────────

#[test]
fn suppressed_only_run_exits_zero() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        // SET lock_timeout keeps require-timeout from firing; only drop-table fires (suppressed).
        .write_stdin(
            "SET lock_timeout = '5s';\n-- pgsafe:ignore drop-table  empty, confirmed\nDROP TABLE x;",
        )
        .assert()
        .success()
        .stdout(predicate::str::contains("suppressed"));
}
#[test]
fn hygiene_diagnostic_gates_exit_one() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("-- pgsafe:ignore drop-table\nDROP TABLE x;")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("suppression-missing-reason"));
}
#[test]
fn json_output_includes_suppression_reason() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--format", "json"])
        // SET lock_timeout keeps require-timeout from firing; only drop-table fires (suppressed).
        .write_stdin(
            "SET lock_timeout = '5s';\n-- pgsafe:ignore drop-table  empty, confirmed\nDROP TABLE x;",
        )
        .assert()
        .success()
        .stdout(predicate::str::contains("\"suppression\""))
        .stdout(predicate::str::contains("empty, confirmed"));
}

// ── old JSON substring test (kept for backwards compat) ──────────────────────

#[test]
fn json_format_emits_rule_id_and_file() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--format", "json"])
        .write_stdin("CREATE INDEX i ON t (x);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("\"add-index-non-concurrent\""))
        .stdout(predicate::str::contains("\"file\""));
}

// ── --fail-on gating ─────────────────────────────────────────────────────────

#[test]
fn fail_on_default_gates_on_a_warning() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("DROP TABLE x;")
        .assert()
        .failure()
        .code(1);
}

#[test]
fn fail_on_error_does_not_gate_on_a_warning() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "error"])
        .write_stdin("DROP TABLE x;")
        .assert()
        .success()
        .stdout(predicate::str::contains("drop-table")); // still printed, just not gating
}

#[test]
fn fail_on_error_gates_on_an_error() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "error"])
        .write_stdin("VACUUM FULL t;")
        .assert()
        .failure()
        .code(1);
}

#[test]
fn fail_on_never_gates_on_nothing() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "never"])
        .write_stdin("VACUUM FULL t;")
        .assert()
        .success()
        .stdout(predicate::str::contains("vacuum-full-cluster"));
}

#[test]
fn fail_on_never_still_exits_2_on_parse_error() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "never"])
        .write_stdin("ALTER TABLE;")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn invalid_fail_on_value_is_a_usage_error() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "bogus"])
        .write_stdin("DROP TABLE x;")
        .assert()
        .failure()
        .code(2);
}

// ── --in-transaction flag ────────────────────────────────────────────────────

#[test]
fn in_transaction_flag_flags_top_level_concurrently() {
    // Without the flag: a top-level CONCURRENTLY index is NOT a concurrently-in-transaction finding.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .write_stdin("CREATE INDEX CONCURRENTLY i ON t (x);")
        .assert()
        .stdout(predicate::str::contains("concurrently-in-transaction").not());

    // With --in-transaction: it IS flagged (exit 1).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--in-transaction"])
        .write_stdin("CREATE INDEX CONCURRENTLY i ON t (x);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("concurrently-in-transaction"));
}

// ── --fail-on × suppression-diagnostic gating ───────────────────────────────

#[test]
fn fail_on_error_gates_on_a_hygiene_error_but_not_on_unused() {
    // A reasonless directive emits suppression-missing-reason (error) → gates under --fail-on=error.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "error"])
        .write_stdin("-- pgsafe:ignore drop-table\nDROP TABLE x;")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("suppression-missing-reason"));
    // A stale directive emits suppression-unused (warning) → does NOT gate under --fail-on=error.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--fail-on", "error"])
        .write_stdin("-- pgsafe:ignore truncate  stale\nDELETE FROM x;")
        .assert()
        .success()
        .stdout(predicate::str::contains("suppression-unused"));
}

#[test]
fn require_primary_key_enabled_via_config_fires() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("pgsafe.toml");
    std::fs::write(&cfg, "[rules]\nrequire-primary-key = true\n").unwrap();

    // Without the config: no finding (off by default).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--no-config")
        .write_stdin("CREATE TABLE t (id int);")
        .assert()
        .success()
        .stdout(predicate::str::contains("require-primary-key").not());

    // With the config enabling it: the finding appears and gates (exit 1).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--config", cfg.to_str().unwrap()])
        .write_stdin("CREATE TABLE t (id int);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("require-primary-key"));
}

#[test]
fn forbidden_column_type_via_config_fires() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("pgsafe.toml");
    std::fs::write(&cfg, "[forbidden-types]\ntimestamp = \"timestamptz\"\n").unwrap();

    // Without the config: no finding.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--no-config")
        .write_stdin("CREATE TABLE t (created timestamp);")
        .assert()
        .success()
        .stdout(predicate::str::contains("forbidden-column-type").not());

    // With the config: the finding appears and gates (exit 1).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--config", cfg.to_str().unwrap()])
        .write_stdin("CREATE TABLE t (created timestamp);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("forbidden-column-type"));
}

#[test]
fn require_not_null_enabled_via_config_fires() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("pgsafe.toml");
    std::fs::write(&cfg, "[rules]\nrequire-not-null = true\n").unwrap();

    // Without the config: no finding (off by default).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--no-config")
        .write_stdin("CREATE TABLE t (email text);")
        .assert()
        .success()
        .stdout(predicate::str::contains("require-not-null").not());

    // With the config enabling it: the finding appears and gates (exit 1).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--config", cfg.to_str().unwrap()])
        .write_stdin("CREATE TABLE t (email text);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("require-not-null"));
}

#[test]
fn version_flag_prints_the_crate_version() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn require_columns_via_config_fires() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("pgsafe.toml");
    std::fs::write(&cfg, "required-columns = [\"created_at\"]\n").unwrap();

    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--no-config")
        .write_stdin("CREATE TABLE t (id int);")
        .assert()
        .success()
        .stdout(predicate::str::contains("require-columns").not());

    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--config", cfg.to_str().unwrap()])
        .write_stdin("CREATE TABLE t (id int);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("require-columns"));
}

#[test]
fn naming_convention_via_config_fires() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("pgsafe.toml");
    std::fs::write(&cfg, "[naming]\ntable = \"^t_\"\n").unwrap();

    // Without the config: no finding.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .arg("--no-config")
        .write_stdin("CREATE TABLE users (id int);")
        .assert()
        .success()
        .stdout(predicate::str::contains("naming-convention").not());

    // With the [naming] config: a mismatching table name is flagged and gates (exit 1).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .args(["--config", cfg.to_str().unwrap()])
        .write_stdin("CREATE TABLE users (id int);")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("naming-convention"));
}

// ── `paths` scoping (§10 of the LSP design doc — CLI addendum) ───────────────
//
// The CLI now consults the same `Config::in_scope` the LSP uses: a file input that
// doesn't match the config's `paths` globs is silently dropped before linting.
// Stdin is exempt (no path identity), and the filter is a no-op when `paths`
// is unset — existing invocations without `paths` must lint exactly as before.

#[test]
fn paths_scoping_lints_in_scope_file_and_skips_out_of_scope_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".pgsafe.toml"),
        "paths = [\"migrations/**\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("migrations")).unwrap();
    std::fs::create_dir_all(dir.path().join("queries")).unwrap();
    std::fs::write(
        dir.path().join("migrations/0001_add_index.sql"),
        "CREATE INDEX idx ON t (col);\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("queries/report.sql"),
        "CREATE INDEX idx ON t (col);\n",
    )
    .unwrap();

    // In scope (matches `paths`): linted normally, findings gate the run.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .current_dir(dir.path())
        .arg("migrations/0001_add_index.sql")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("add-index-non-concurrent"));

    // Out of scope: silently dropped before linting, exits clean, and prints no
    // note (silent by default — an out-of-scope file produces no output at all).
    Command::cargo_bin("pgsafe")
        .unwrap()
        .current_dir(dir.path())
        .arg("queries/report.sql")
        .assert()
        .success()
        .stdout(predicate::str::contains("add-index-non-concurrent").not())
        .stderr(predicate::str::contains("skipped").not());
}

#[test]
fn paths_scoping_never_filters_stdin() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".pgsafe.toml"),
        "paths = [\"migrations/**\"]\n",
    )
    .unwrap();

    // Piped SQL has no path identity, so it's linted regardless of `paths`.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .current_dir(dir.path())
        .write_stdin("CREATE INDEX idx ON t (col);\n")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("add-index-non-concurrent"));
}

#[test]
fn paths_scoping_is_a_noop_when_unset() {
    // Control: with no `paths` key (here, no config at all via --no-config), a file
    // shaped like the "out of scope" fixture above is still linted — proving the
    // filter changes nothing for the common case of a project that never sets `paths`.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("queries")).unwrap();
    std::fs::write(
        dir.path().join("queries/report.sql"),
        "CREATE INDEX idx ON t (col);\n",
    )
    .unwrap();

    Command::cargo_bin("pgsafe")
        .unwrap()
        .current_dir(dir.path())
        .arg("--no-config")
        .arg("queries/report.sql")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("add-index-non-concurrent"));
}

// ── color / summary ──────────────────────────────────────────────────────────

#[test]
fn human_default_piped_is_plain_but_has_summary() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .env_remove("CLICOLOR_FORCE")
        .env_remove("NO_COLOR")
        .write_stdin("VACUUM FULL t;")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains(
            "Summary: 1 error, 1 warning in 1 file",
        ))
        .stdout(predicate::str::contains('\u{1b}').not())
        .stdout(predicate::str::contains('✗').not());
}

#[test]
fn color_always_adds_escapes_and_glyph() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .env_remove("CLICOLOR_FORCE")
        .env_remove("NO_COLOR")
        .args(["--color", "always"])
        .write_stdin("VACUUM FULL t;")
        .assert()
        .failure()
        .stdout(predicate::str::contains('\u{1b}'))
        .stdout(predicate::str::contains('✗'));
}

#[test]
fn color_never_stays_plain() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .env_remove("CLICOLOR_FORCE")
        .env_remove("NO_COLOR")
        .args(["--color", "never"])
        .write_stdin("VACUUM FULL t;")
        .assert()
        .failure()
        .stdout(predicate::str::contains('\u{1b}').not());
}

#[test]
fn no_color_env_disables_auto_but_always_overrides() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .write_stdin("VACUUM FULL t;")
        .assert()
        .stdout(predicate::str::contains('\u{1b}').not());
    Command::cargo_bin("pgsafe")
        .unwrap()
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .args(["--color", "always"])
        .write_stdin("VACUUM FULL t;")
        .assert()
        .stdout(predicate::str::contains('\u{1b}'));
}

#[test]
fn clicolor_force_colors_piped_output() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .env_remove("NO_COLOR")
        .env("CLICOLOR_FORCE", "1")
        .write_stdin("VACUUM FULL t;")
        .assert()
        .stdout(predicate::str::contains('\u{1b}'));
}

#[test]
fn clicolor_force_beats_no_color_under_auto() {
    // Both set, default --color auto: CLICOLOR_FORCE is checked first, so color wins.
    Command::cargo_bin("pgsafe")
        .unwrap()
        .env("CLICOLOR_FORCE", "1")
        .env("NO_COLOR", "1")
        .write_stdin("VACUUM FULL t;")
        .assert()
        .stdout(predicate::str::contains('\u{1b}'));
}

#[test]
fn clean_run_prints_nothing_to_stdout() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .env_remove("CLICOLOR_FORCE")
        .env_remove("NO_COLOR")
        .write_stdin("CREATE INDEX CONCURRENTLY i ON t (x);")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn color_flag_does_not_affect_json() {
    Command::cargo_bin("pgsafe")
        .unwrap()
        .env_remove("CLICOLOR_FORCE")
        .env_remove("NO_COLOR")
        .args(["--color", "always", "--format", "json"])
        .write_stdin("VACUUM FULL t;")
        .assert()
        .stdout(predicate::str::contains('\u{1b}').not());
}
