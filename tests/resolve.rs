use clap::Parser;
use pgsafe::cli::{resolve, Cli};
use pgsafe::FailOn;

#[test]
fn resolve_returns_inputs_settings_and_options() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("m.sql");
    std::fs::write(&f, "DROP TABLE x;\n").unwrap();

    // --no-config avoids config discovery (so no dependency on the process CWD); an absolute
    // path is readable from anywhere.
    let cli = Cli::try_parse_from([
        "pgsafe",
        "--no-config",
        "--fail-on",
        "error",
        f.to_str().unwrap(),
    ])
    .unwrap();
    let r = resolve(&cli.args).unwrap();

    assert_eq!(r.inputs.len(), 1);
    assert_eq!(r.inputs[0].1, "DROP TABLE x;\n");
    assert_eq!(r.fail_on, FailOn::Error);
    // No config → no disabled rules in the per-file options.
    assert!(r.options_for(&r.inputs[0].0).disabled_rules.is_empty());
}

#[test]
fn options_for_reflects_config_disabled_rules() {
    // The contract pgsafe-pro depends on: a config rule-disable shows up in the per-file options.
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join(".pgsafe.toml");
    std::fs::write(&cfg, "[rules]\ndrop-table = false\n").unwrap();
    let f = dir.path().join("m.sql");
    std::fs::write(&f, "DROP TABLE x;\n").unwrap();

    let cli = Cli::try_parse_from([
        "pgsafe",
        "--config",
        cfg.to_str().unwrap(),
        f.to_str().unwrap(),
    ])
    .unwrap();
    let r = resolve(&cli.args).unwrap();

    assert!(r
        .options_for(&r.inputs[0].0)
        .disabled_rules
        .contains("drop-table"));
}
