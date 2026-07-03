//! `pgsafe.toml` (or `.pgsafe.toml`) config: discovery, parsing, strict validation, and per-file
//! resolution into the rule settings the engine consumes. Only built with the
//! `cli` feature. The `Config` type is a format-neutral serde target; the loader
//! dispatches on file extension so a future YAML format is one match arm away.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use serde::Deserialize;

use crate::{FailOn, Format, NameKind, Severity};

/// The candidate config filenames, in priority order (v1: TOML only).
/// Config file names discovery looks for, in precedence order: the plain
/// `pgsafe.toml` first (preferred — dotfiles are easy to overlook and many tools
/// skip them), then the hidden `.pgsafe.toml`. If a directory holds both, the
/// non-dotfile wins.
const CANDIDATES: &[&str] = &["pgsafe.toml", ".pgsafe.toml"];

/// The annotated example config (`pgsafe --example-config`). Every option is shown, commented
/// out, so the file is a valid no-op as-is. Kept in sync with the schema by `example_config_*`
/// tests below.
pub(crate) const EXAMPLE_CONFIG: &str = include_str!("../../config-example.toml");

/// A config problem. Rendered by the CLI as `error: {0}` and mapped to exit 2.
#[derive(Debug)]
pub(crate) struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A `[rules]` value: enable/disable, or force a severity. Custom `Deserialize`
/// (not `#[serde(untagged)]`) for robust TOML handling and clear error messages.
#[derive(Debug, Clone, Copy)]
enum RuleSetting {
    Enabled(bool),
    Severity(Severity),
}

impl<'de> Deserialize<'de> for RuleSetting {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = RuleSetting;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(r#"a boolean, or "error"/"warning""#)
            }
            fn visit_bool<E: serde::de::Error>(self, b: bool) -> Result<RuleSetting, E> {
                Ok(RuleSetting::Enabled(b))
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<RuleSetting, E> {
                match s {
                    "error" => Ok(RuleSetting::Severity(Severity::Error)),
                    "warning" => Ok(RuleSetting::Severity(Severity::Warning)),
                    other => Err(E::custom(format!(
                        r#"expected a boolean or "error"/"warning", got "{other}""#
                    ))),
                }
            }
        }
        d.deserialize_any(V)
    }
}

/// An `[[ignore]]` entry as written on disk.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIgnore {
    path: String,
    #[serde(default = "ignore_all")]
    rules: Vec<String>,
}

fn ignore_all() -> Vec<String> {
    vec!["*".to_string()]
}

/// The `[naming]` section: one optional regex per identifier kind.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamingConfig {
    table: Option<String>,
    column: Option<String>,
    index: Option<String>,
    constraint: Option<String>,
    sequence: Option<String>,
    trigger: Option<String>,
    schema: Option<String>,
}

/// The on-disk config shape. `deny_unknown_fields` makes a typo'd key a hard error.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    fail_on: Option<FailOn>,
    in_transaction: Option<bool>,
    format: Option<Format>,
    since: Option<String>,
    #[serde(default)]
    rules: BTreeMap<String, RuleSetting>,
    #[serde(default)]
    ignore: Vec<RawIgnore>,
    #[serde(default)]
    naming: NamingConfig,
    #[serde(default, rename = "forbidden-types")]
    forbidden_types: BTreeMap<String, String>,
    #[serde(default, rename = "required-columns")]
    required_columns: Vec<String>,
}

/// A validated, compiled config ready for per-file resolution.
#[derive(Debug, Default)]
pub(crate) struct Config {
    pub fail_on: Option<FailOn>,
    pub in_transaction: Option<bool>,
    pub format: Option<Format>,
    /// Cutoff path for `--since`-style filtering: lint only files whose path sorts after this.
    pub since: Option<String>,
    disabled: BTreeSet<String>,            // global `[rules] = false`
    overrides: BTreeMap<String, Severity>, // global `[rules] = "sev"`
    enabled: BTreeSet<String>,             // global `[rules] = true` / `= "sev"`
    ignores: Vec<(globset::GlobMatcher, BTreeSet<String>)>, // compiled glob -> ids ("*" expanded)
    naming: BTreeMap<NameKind, String>,
    forbidden_types: BTreeMap<String, String>,
    required_columns: BTreeSet<String>,
}

/// Walk up from `start` to the first directory holding a candidate config file,
/// stopping at a `.git` boundary or the filesystem root. Returns the file path.
pub(crate) fn discover(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        for name in CANDIDATES {
            let candidate = d.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if d.join(".git").exists() {
            return None; // at the repo root; don't ascend past it
        }
        dir = d.parent();
    }
    None
}

/// Read, parse, and validate the config at `path`, compiling it against the set of
/// known rule ids. Any problem is a `ConfigError`.
pub(crate) fn load(path: &Path, known: &[&str]) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError(format!("{}: {e}", path.display())))?;
    let raw: RawConfig = match path.extension().and_then(|e| e.to_str()) {
        Some("toml") | None => {
            toml::from_str(&text).map_err(|e| ConfigError(format!("{}: {e}", path.display())))?
        }
        Some(other) => {
            return Err(ConfigError(format!(
                "{}: unsupported config format `.{other}`",
                path.display()
            )));
        }
    };
    compile(raw, known)
}

/// Parse a TOML string directly (used by tests).
#[cfg(test)]
fn from_toml_str(text: &str, known: &[&str]) -> Result<Config, ConfigError> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| ConfigError(e.to_string()))?;
    compile(raw, known)
}

fn compile(raw: RawConfig, known: &[&str]) -> Result<Config, ConfigError> {
    let is_known = |id: &str| known.contains(&id);

    let mut disabled = BTreeSet::new();
    let mut overrides = BTreeMap::new();
    let mut enabled = BTreeSet::new();
    for (id, setting) in &raw.rules {
        if !is_known(id) {
            return Err(ConfigError(format!("[rules] targets unknown rule `{id}`")));
        }
        match setting {
            RuleSetting::Enabled(true) => {
                enabled.insert(id.clone());
            }
            RuleSetting::Enabled(false) => {
                disabled.insert(id.clone());
            }
            RuleSetting::Severity(s) => {
                overrides.insert(id.clone(), *s);
                enabled.insert(id.clone());
            }
        }
    }

    let mut ignores = Vec::new();
    for ig in &raw.ignore {
        let matcher = GlobBuilder::new(&ig.path)
            .literal_separator(true)
            .build()
            .map_err(|e| ConfigError(format!("[[ignore]] invalid glob `{}`: {e}", ig.path)))?
            .compile_matcher();
        let mut rules = BTreeSet::new();
        for r in &ig.rules {
            if r == "*" {
                rules.extend(known.iter().map(|s| (*s).to_string()));
            } else if is_known(r) {
                rules.insert(r.clone());
            } else {
                return Err(ConfigError(format!(
                    "[[ignore]] rules list targets unknown rule `{r}`"
                )));
            }
        }
        ignores.push((matcher, rules));
    }

    let mut naming = BTreeMap::new();
    for (kind, pat) in [
        (NameKind::Table, &raw.naming.table),
        (NameKind::Column, &raw.naming.column),
        (NameKind::Index, &raw.naming.index),
        (NameKind::Constraint, &raw.naming.constraint),
        (NameKind::Sequence, &raw.naming.sequence),
        (NameKind::Trigger, &raw.naming.trigger),
        (NameKind::Schema, &raw.naming.schema),
    ] {
        if let Some(p) = pat {
            regex::Regex::new(p).map_err(|e| {
                ConfigError(format!(
                    "[naming] {} is not a valid regex: {e}",
                    kind.as_str()
                ))
            })?;
            naming.insert(kind, p.clone());
        }
    }

    Ok(Config {
        fail_on: raw.fail_on,
        in_transaction: raw.in_transaction,
        format: raw.format,
        since: raw.since,
        disabled,
        overrides,
        enabled,
        ignores,
        naming,
        forbidden_types: raw.forbidden_types,
        // Stored verbatim; the `require-columns` rule folds names to lower case (matching
        // PostgreSQL's unquoted-identifier folding) and skips empties at match time.
        required_columns: raw.required_columns.into_iter().collect(),
    })
}

impl Config {
    /// The set of rule ids disabled for `rel_path` — the global disables plus every
    /// `[[ignore]]` whose glob matches (union; `"*"` already expanded at load).
    pub(crate) fn disabled_for(&self, rel_path: &str) -> BTreeSet<String> {
        let mut d = self.disabled.clone();
        for (matcher, rules) in &self.ignores {
            if matcher.is_match(rel_path) {
                d.extend(rules.iter().cloned());
            }
        }
        d
    }

    /// Global severity overrides (same for every file).
    pub(crate) fn overrides(&self) -> &BTreeMap<String, Severity> {
        &self.overrides
    }

    /// Rule ids explicitly enabled in config (global). Opt-in rules run only when their id is here.
    pub(crate) fn enabled(&self) -> &BTreeSet<String> {
        &self.enabled
    }

    /// Per-kind naming-convention patterns (global).
    pub(crate) fn naming(&self) -> &BTreeMap<NameKind, String> {
        &self.naming
    }

    /// Forbidden column types → suggested replacement (global).
    pub(crate) fn forbidden_types(&self) -> &BTreeMap<String, String> {
        &self.forbidden_types
    }

    /// Column names every CREATE TABLE must include (global).
    pub(crate) fn required_columns(&self) -> &BTreeSet<String> {
        &self.required_columns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOWN: &[&str] = &[
        "drop-table",
        "truncate",
        "add-index-non-concurrent",
        "require-timeout",
    ];

    #[test]
    fn example_config_parses_and_is_current() {
        // The shipped template (what `pgsafe --example-config` prints) must parse cleanly
        // against the real rule set — a broken TOML edit fails here, not in a user's setup.
        let known = crate::known_rule_ids();
        let parsed = from_toml_str(EXAMPLE_CONFIG, &known);
        assert!(
            parsed.is_ok(),
            "example config must parse: {:?}",
            parsed.err()
        );
        // Every rule the example documents must still exist (a rename fails here) and must be
        // present in the template (so the example can't quietly drop a documented rule).
        for rule in [
            "drop-table",
            "add-trigger",
            "add-index-non-concurrent",
            "require-primary-key",
            "require-not-null",
            "require-comment",
            "require-if-exists",
            "forbid-nullable-fk",
            "unchecked-do-block",
            "naming-convention",
            "forbidden-column-type",
            "require-columns",
        ] {
            assert!(
                known.contains(&rule),
                "example references unknown rule `{rule}` (renamed or removed?)"
            );
            assert!(
                EXAMPLE_CONFIG.contains(rule),
                "example config should mention `{rule}`"
            );
        }
    }

    #[test]
    fn parses_scalars_rules_and_ignores() {
        let cfg = from_toml_str(
            r#"
            fail_on = "error"
            in_transaction = true
            format = "json"
            [rules]
            drop-table = false
            add-index-non-concurrent = "warning"
            [[ignore]]
            path = "legacy/**"
            rules = ["truncate"]
        "#,
            KNOWN,
        )
        .unwrap();
        assert_eq!(cfg.fail_on, Some(FailOn::Error));
        assert_eq!(cfg.in_transaction, Some(true));
        assert_eq!(cfg.format, Some(Format::Json));
        assert!(cfg.disabled_for("src/001.sql").contains("drop-table"));
        assert_eq!(
            cfg.overrides().get("add-index-non-concurrent"),
            Some(&Severity::Warning)
        );
    }

    #[test]
    fn parses_the_since_cutoff() {
        let cfg = from_toml_str("since = \"db/migrate/0042.sql\"\n", KNOWN).unwrap();
        assert_eq!(cfg.since.as_deref(), Some("db/migrate/0042.sql"));
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = from_toml_str("fail_onn = \"error\"\n", KNOWN).unwrap_err();
        assert!(err.0.contains("fail_onn") || err.0.contains("unknown"));
    }

    #[test]
    fn unknown_rule_id_is_rejected() {
        let err = from_toml_str("[rules]\ndrop-tabel = false\n", KNOWN).unwrap_err();
        assert!(err.0.contains("drop-tabel"));
    }

    #[test]
    fn bad_rule_value_is_rejected() {
        let err = from_toml_str("[rules]\ndrop-table = \"sometimes\"\n", KNOWN).unwrap_err();
        assert!(err.0.contains("error") && err.0.contains("warning"));
    }

    #[test]
    fn ignore_rules_default_to_all() {
        let cfg = from_toml_str("[[ignore]]\npath = \"legacy/**\"\n", KNOWN).unwrap();
        let d = cfg.disabled_for("legacy/001.sql");
        assert!(d.contains("drop-table") && d.contains("require-timeout")); // "*" expanded
    }

    #[test]
    fn ignore_union_and_non_match() {
        let cfg = from_toml_str(
            r#"
            [[ignore]]
            path = "legacy/**"
            rules = ["drop-table"]
            [[ignore]]
            path = "**/*_seed.sql"
            rules = ["truncate"]
        "#,
            KNOWN,
        )
        .unwrap();
        let legacy_seed = cfg.disabled_for("legacy/9_seed.sql");
        assert!(legacy_seed.contains("drop-table") && legacy_seed.contains("truncate"));
        assert!(cfg.disabled_for("current/1.sql").is_empty());
    }

    #[test]
    fn invalid_glob_is_rejected() {
        // "a[" has an unterminated character class — definitely rejected by globset.
        let err =
            from_toml_str("[[ignore]]\npath = \"a[\"\nrules=[\"truncate\"]\n", KNOWN).unwrap_err();
        assert!(err.0.contains("glob"));
    }

    #[test]
    fn ignore_with_unknown_rule_is_rejected() {
        let err = from_toml_str(
            "[[ignore]]\npath = \"legacy/**\"\nrules = [\"typo-rule\"]\n",
            KNOWN,
        )
        .unwrap_err();
        assert!(err.0.contains("typo-rule"));
    }

    #[test]
    fn rules_true_enables_a_rule() {
        let cfg = from_toml_str("[rules]\ndrop-table = true\n", KNOWN).unwrap();
        assert!(cfg.enabled().contains("drop-table"));
        assert!(!cfg.disabled_for("any.sql").contains("drop-table"));
    }

    #[test]
    fn rules_severity_enables_and_overrides() {
        let cfg = from_toml_str("[rules]\ndrop-table = \"error\"\n", KNOWN).unwrap();
        assert!(cfg.enabled().contains("drop-table"));
        assert_eq!(
            cfg.overrides().get("drop-table"),
            Some(&crate::Severity::Error)
        );
    }

    #[test]
    fn rules_false_disables_not_enables() {
        let cfg = from_toml_str("[rules]\ndrop-table = false\n", KNOWN).unwrap();
        assert!(!cfg.enabled().contains("drop-table"));
        assert!(cfg.disabled_for("any.sql").contains("drop-table"));
    }

    #[test]
    fn discover_walks_up_and_stops_at_git() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join(".pgsafe.toml"), "fail_on = \"error\"\n").unwrap();
        let deep = root.path().join("db/migrations");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            discover(&deep).as_deref(),
            Some(root.path().join(".pgsafe.toml").as_path())
        );

        // No config, and a .git boundary stops the walk before the parent.
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(other.path().join(".git")).unwrap();
        let sub = other.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(discover(&sub), None);
    }

    #[test]
    fn discover_finds_the_non_dotfile_name() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join("pgsafe.toml"), "fail_on = \"error\"\n").unwrap();
        let deep = root.path().join("db/migrations");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            discover(&deep).as_deref(),
            Some(root.path().join("pgsafe.toml").as_path())
        );
    }

    #[test]
    fn naming_section_compiles_patterns() {
        let cfg = from_toml_str("[naming]\ntable = \"^t_\"\nindex = \"^ix_\"\n", KNOWN).unwrap();
        assert_eq!(
            cfg.naming()
                .get(&crate::NameKind::Table)
                .map(String::as_str),
            Some("^t_")
        );
        assert_eq!(
            cfg.naming()
                .get(&crate::NameKind::Index)
                .map(String::as_str),
            Some("^ix_")
        );
        assert!(cfg.naming().get(&crate::NameKind::Column).is_none());
    }

    #[test]
    fn naming_malformed_regex_is_a_config_error() {
        let err = from_toml_str("[naming]\ntable = \"^(unclosed\"\n", KNOWN).unwrap_err();
        assert!(
            err.0.contains("table"),
            "error should name the kind: {}",
            err.0
        );
    }

    #[test]
    fn naming_unknown_kind_is_a_config_error() {
        assert!(from_toml_str("[naming]\ntabel = \"^t_\"\n", KNOWN).is_err());
    }

    #[test]
    fn forbidden_types_section_compiles() {
        let cfg = from_toml_str("[forbidden-types]\ntimestamp = \"timestamptz\"\n", KNOWN).unwrap();
        assert_eq!(
            cfg.forbidden_types().get("timestamp").map(String::as_str),
            Some("timestamptz")
        );
    }

    #[test]
    fn forbidden_types_unknown_type_compiles_without_error() {
        // No type-existence validation: an unrecognized type is carried verbatim (it is inert at
        // match time, never a config error).
        let cfg = from_toml_str("[forbidden-types]\nnotatype = \"text\"\n", KNOWN).unwrap();
        assert!(cfg.forbidden_types().contains_key("notatype"));
    }

    #[test]
    fn required_columns_section_compiles() {
        let cfg = from_toml_str(
            "required-columns = [\"created_at\", \"updated_at\"]\n",
            KNOWN,
        )
        .unwrap();
        assert!(cfg.required_columns().contains("created_at"));
        assert!(cfg.required_columns().contains("updated_at"));
    }

    #[test]
    fn required_columns_stored_verbatim() {
        // Config stores names as written; the require-columns rule folds case + skips empties.
        let cfg = from_toml_str("required-columns = [\"Created_At\"]\n", KNOWN).unwrap();
        assert!(cfg.required_columns().contains("Created_At"));
    }

    #[test]
    fn required_columns_absent_is_empty() {
        let cfg = from_toml_str("", KNOWN).unwrap();
        assert!(cfg.required_columns().is_empty());
    }

    #[test]
    fn discover_prefers_the_non_dotfile_when_both_exist() {
        // A directory holding both names is ambiguous; the visible `pgsafe.toml` wins.
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join("pgsafe.toml"), "fail_on = \"error\"\n").unwrap();
        std::fs::write(root.path().join(".pgsafe.toml"), "fail_on = \"never\"\n").unwrap();
        assert_eq!(
            discover(root.path()).as_deref(),
            Some(root.path().join("pgsafe.toml").as_path())
        );
    }
}
