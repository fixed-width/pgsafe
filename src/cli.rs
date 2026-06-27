//! Command-line surface (only built with the `cli` feature). A superset binary
//! can `#[command(flatten)]` [`CommonArgs`] and reuse [`run`] or its building
//! blocks instead of re-implementing argument parsing, rendering, and gating.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::{
    config, gate, lint_input, render_errors, render_human, render_json, FailOn, FileReport, Format,
};

/// The flags shared by every pgsafe-style CLI. Flatten this into a larger
/// `clap` parser to inherit them.
#[non_exhaustive]
#[derive(clap::Args)]
pub struct CommonArgs {
    /// SQL files to lint; use '-' or omit to read from stdin.
    pub paths: Vec<String>,
    /// Output format. (Default: human; overrides the config's `format`.)
    #[arg(long, value_enum)]
    pub format: Option<Format>,
    /// Minimum finding severity that fails the run. (Default: warning; overrides the config's `fail_on`.)
    #[arg(long, value_enum)]
    pub fail_on: Option<FailOn>,
    /// Treat each input as already running inside a transaction.
    #[arg(long)]
    pub in_transaction: bool,
    /// Use this exact config file (skips discovery).
    #[arg(long, value_name = "PATH", conflicts_with = "no_config")]
    pub config: Option<PathBuf>,
    /// Ignore any `.pgsafe.toml`; use built-in defaults + CLI flags only.
    #[arg(long)]
    pub no_config: bool,
    /// Lint only the `.sql` files added/modified versus this git ref (e.g. `origin/main`).
    /// Positional paths become a git pathspec scope; with no paths, the whole repository.
    #[arg(long, value_name = "REF")]
    pub git_diff: Option<String>,
    /// Lint only migration files whose path sorts after this cutoff (the last legacy migration).
    /// Also settable as `since = "..."` in `.pgsafe.toml`; this flag overrides it.
    #[arg(long, value_name = "CUTOFF", conflicts_with = "git_diff")]
    pub since: Option<String>,
}

/// The `pgsafe` binary's top-level parser.
#[non_exhaustive]
#[derive(clap::Parser)]
#[command(
    name = "pgsafe",
    about = "Lint PostgreSQL DDL migrations for unsafe operations"
)]
pub struct Cli {
    /// The shared linting flags.
    #[command(flatten)]
    pub args: CommonArgs,
}

/// Read, lint, render, and gate the inputs in `args`, returning the process
/// exit code (`0` clean, `1` gated findings, `2` parse/IO error).
#[must_use]
pub fn run(args: CommonArgs) -> ExitCode {
    let (config, config_dir) = match load_config(&args) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::from(2);
        }
    };

    let inputs = match &args.git_diff {
        Some(reference) => {
            if args.paths.iter().any(|p| p == "-") {
                eprintln!("error: `-` (stdin) cannot be combined with --git-diff");
                return ExitCode::from(2);
            }
            let files = match crate::gitdiff::changed_sql_files(reference, &args.paths) {
                Ok(f) => f,
                Err(msg) => {
                    eprintln!("error: {msg}");
                    return ExitCode::from(2);
                }
            };
            match read_files(&files) {
                Ok(i) => i,
                Err(msg) => {
                    eprintln!("error: {msg}");
                    return ExitCode::from(2);
                }
            }
        }
        None => {
            // `--since` (CLI) or a config `since` filters the explicit file paths by full-path
            // ordering, before reading. With no paths (stdin) `since` does not apply.
            let effective_since = args.since.clone().or(config.since.clone());
            let result = match effective_since {
                Some(cutoff) if !args.paths.is_empty() => {
                    let kept: Vec<PathBuf> = filter_since(&args.paths, &cutoff)
                        .iter()
                        .map(PathBuf::from)
                        .collect();
                    read_files(&kept)
                }
                _ => read_inputs(&args.paths),
            };
            match result {
                Ok(i) => i,
                Err(msg) => {
                    eprintln!("error: {msg}");
                    return ExitCode::from(2);
                }
            }
        }
    };

    let fail_on = args.fail_on.or(config.fail_on).unwrap_or(FailOn::Warning);
    let format = args
        .format
        .clone()
        .or(config.format.clone())
        .unwrap_or(Format::Human);
    let assume_in_transaction = args.in_transaction || config.in_transaction.unwrap_or(false);
    let overrides = config.overrides().clone();

    let reports: Vec<FileReport> = inputs
        .into_iter()
        .map(|(name, sql)| {
            let rel = rel_path(&name, config_dir.as_deref());
            let options = crate::LintOptions {
                assume_in_transaction,
                disabled_rules: config.disabled_for(&rel),
                severity_overrides: overrides.clone(),
                ..crate::LintOptions::default()
            };
            lint_input(name, &sql, &options)
        })
        .collect();

    let had_error = reports.iter().any(|r| r.error.is_some());
    let had_findings = reports.iter().any(|r| gate(&r.findings, fail_on));

    match format {
        Format::Human => {
            eprint!("{}", render_errors(&reports));
            print!("{}", render_human(&reports));
        }
        Format::Json => match render_json(&reports) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        },
    }

    if had_error {
        ExitCode::from(2)
    } else if had_findings {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Resolve the active config and the directory it lives in (for relative-path matching).
/// `--no-config` or "no file discovered" yields an empty default config.
fn load_config(args: &CommonArgs) -> Result<(config::Config, Option<PathBuf>), String> {
    if args.no_config {
        return Ok((config::Config::default(), None));
    }
    let path = match &args.config {
        Some(p) => Some(p.clone()),
        None => {
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            config::discover(&cwd)
        }
    };
    match path {
        Some(p) => {
            let known = crate::known_rule_ids();
            let cfg = config::load(&p, &known).map_err(|e| e.to_string())?;
            let dir = p.parent().map(Path::to_path_buf);
            Ok((cfg, dir))
        }
        None => Ok((config::Config::default(), None)),
    }
}

/// A linted file's path made relative to the config dir (for glob matching).
/// Both the file path and the config dir are absolutized against the current
/// working directory first, so an ignore glob written relative to the config
/// dir still matches when pgsafe is invoked from a subdirectory.
fn rel_path(name: &str, config_dir: Option<&Path>) -> String {
    let Some(dir) = config_dir else {
        return name.to_string();
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let abs_name = if Path::new(name).is_absolute() {
        PathBuf::from(name)
    } else {
        cwd.join(name)
    };
    let abs_dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        cwd.join(dir)
    };
    abs_name
        .strip_prefix(&abs_dir)
        .unwrap_or(&abs_name)
        .to_string_lossy()
        .into_owned()
}

fn read_inputs(paths: &[String]) -> Result<Vec<(String, String)>, String> {
    if paths.is_empty() {
        return Ok(vec![("<stdin>".to_string(), read_stdin()?)]);
    }
    let mut out = Vec::new();
    for p in paths {
        if p == "-" {
            out.push(("<stdin>".to_string(), read_stdin()?));
        } else {
            let sql = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
            out.push((p.clone(), sql));
        }
    }
    Ok(out)
}

/// Keep only paths that sort strictly after `cutoff` (lexicographic full-path comparison),
/// so `--since` lints the new migrations and never the legacy ones (path <= cutoff).
fn filter_since(paths: &[String], cutoff: &str) -> Vec<String> {
    paths
        .iter()
        .filter(|p| p.as_str() > cutoff)
        .cloned()
        .collect()
}

/// Read each path into a `(display-name, contents)` pair. Unlike `read_inputs`, an empty
/// list yields no inputs — it never falls back to stdin, so `--git-diff` with no changed
/// files lints nothing rather than blocking on stdin.
fn read_files(paths: &[PathBuf]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for p in paths {
        let sql = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        out.push((p.display().to_string(), sql));
    }
    Ok(out)
}

fn read_stdin() -> Result<String, String> {
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .map_err(|e| e.to_string())?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::filter_since;

    #[test]
    fn filter_since_keeps_paths_strictly_after_cutoff() {
        let paths = vec![
            "db/migrate/0001.sql".to_string(),
            "db/migrate/0042_cut.sql".to_string(),
            "db/migrate/0043.sql".to_string(),
            "db/migrate/0100.sql".to_string(),
        ];
        let kept = filter_since(&paths, "db/migrate/0042_cut.sql");
        assert_eq!(
            kept,
            vec![
                "db/migrate/0043.sql".to_string(),
                "db/migrate/0100.sql".to_string(),
            ]
        );
    }
}
