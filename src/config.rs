//! `pgsafe.toml` (or `.pgsafe.toml`) config: discovery, parsing, strict validation, and per-file
//! resolution into the rule settings the engine consumes. Only built with the
//! `cli` feature. The `Config` type is a format-neutral serde target; the loader
//! dispatches on file extension so a future YAML format is one match arm away.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use serde::Deserialize;

use crate::{FailOn, Format, Severity};

/// The candidate config filenames, in priority order (v1: TOML only).
/// Config file names discovery looks for, in precedence order: the plain
/// `pgsafe.toml` first (preferred — dotfiles are easy to overlook and many tools
/// skip them), then the hidden `.pgsafe.toml`. If a directory holds both, the
/// non-dotfile wins.
const CANDIDATES: &[&str] = &["pgsafe.toml", ".pgsafe.toml"];

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
    ignores: Vec<(globset::GlobMatcher, BTreeSet<String>)>, // compiled glob -> ids ("*" expanded)
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
    for (id, setting) in &raw.rules {
        if !is_known(id) {
            return Err(ConfigError(format!("[rules] targets unknown rule `{id}`")));
        }
        match setting {
            RuleSetting::Enabled(true) => {} // explicit enable: no-op
            RuleSetting::Enabled(false) => {
                disabled.insert(id.clone());
            }
            RuleSetting::Severity(s) => {
                overrides.insert(id.clone(), *s);
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

    Ok(Config {
        fail_on: raw.fail_on,
        in_transaction: raw.in_transaction,
        format: raw.format,
        since: raw.since,
        disabled,
        overrides,
        ignores,
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
