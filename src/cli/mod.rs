//! Command-line surface (only built with the `cli` feature). A superset binary
//! can `#[command(flatten)]` [`CommonArgs`] and call [`resolve`] to reuse the whole
//! front-end — config discovery, `--config`/`--git-diff`/`--since` input selection, and
//! per-file options — then run its own lint/render loop over the returned [`ResolvedRun`].

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::{
    config, gate, lint_input, render_errors, render_github, render_human_styled, render_json,
    render_sarif, render_summary, ColorWhen, FailOn, FileReport, Format, LintOptions, Styling,
};

mod fix;
mod gitdiff;

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
    /// When to colorize human output: `auto` (a TTY, honoring `NO_COLOR` /
    /// `CLICOLOR_FORCE`), `always`, or `never`. Applies only to `--format human`.
    #[arg(long, value_enum, default_value = "auto")]
    pub color: ColorWhen,
    /// Minimum finding severity that fails the run. (Default: warning; overrides the config's `fail_on`.)
    #[arg(long, value_enum)]
    pub fail_on: Option<FailOn>,
    /// Treat each input as already running inside a transaction.
    #[arg(long)]
    pub in_transaction: bool,
    /// Use this exact config file (skips discovery).
    #[arg(long, value_name = "PATH", conflicts_with = "no_config")]
    pub config: Option<PathBuf>,
    /// Ignore any `pgsafe.toml` / `.pgsafe.toml`; use built-in defaults + CLI flags only.
    #[arg(long)]
    pub no_config: bool,
    /// Lint only the `.sql` files added/modified versus this git ref (e.g. `origin/main`).
    /// Positional paths become a git pathspec scope (relative to the repo root); with no paths, the whole repository.
    #[arg(long, value_name = "REF")]
    pub git_diff: Option<String>,
    /// Lint only migration files whose path sorts after this cutoff (the last legacy migration).
    /// Also settable as `since = "..."` in the config file; this flag overrides it.
    #[arg(long, value_name = "CUTOFF", conflicts_with = "git_diff")]
    pub since: Option<String>,
    /// Print an annotated example `.pgsafe.toml` to stdout and exit, e.g.
    /// `pgsafe --example-config > .pgsafe.toml`.
    #[arg(long)]
    pub example_config: bool,
    /// Print the ids of every rule this build can emit (one per line, or a JSON
    /// envelope with `--format json`) and exit.
    #[arg(long)]
    pub list_rules: bool,
    /// Apply fixes in place (files) or to stdout (stdin). Human-output only;
    /// cannot combine with --diff or --format json/github/sarif.
    #[arg(long, conflicts_with = "diff")]
    pub fix: bool,
    /// Preview the fixes --fix would apply as a unified diff; writes nothing.
    #[arg(long)]
    pub diff: bool,
}

/// The `pgsafe` binary's top-level parser.
///
/// `CommonArgs` has a positional `paths: Vec<String>`; `command` adds an optional
/// subcommand alongside it. clap resolves the ambiguity itself: a leading token that
/// matches a known subcommand name (e.g. `lsp`) dispatches to `Command`, and anything
/// else — including a path that happens to look like one, since `Vec<String>` never
/// requires a value — falls through to `CommonArgs::paths` as before. Verified for
/// `pgsafe <path>`, bare `pgsafe` (stdin), `pgsafe lsp`/`pgsafe lsp --help`, and the
/// existing flag suite (`--git-diff`, `--list-rules`, …); see `tests/cli.rs`.
#[non_exhaustive]
#[derive(clap::Parser)]
#[command(
    name = "pgsafe",
    version,
    about = "Lint PostgreSQL DDL migrations for unsafe operations"
)]
pub struct Cli {
    /// The shared linting flags (used when no subcommand is given).
    #[command(flatten)]
    pub args: CommonArgs,

    /// The subcommand to run, if any (absence runs the default lint over `args`).
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Subcommands beyond the default lint run.
#[non_exhaustive]
#[derive(clap::Subcommand)]
pub enum Command {
    /// Run the language server over stdio (for editor integration).
    #[cfg(feature = "lsp")]
    Lsp,
}

/// Entry the `pgsafe` binary calls with the fully-parsed CLI. Routes to a
/// subcommand if present, else runs the default lint over `args`.
#[must_use]
pub fn main_entry(cli: Cli) -> ExitCode {
    match cli.command {
        #[cfg(feature = "lsp")]
        Some(Command::Lsp) => match crate::lsp::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        // A wildcard, not `None`: in a `cli`-only build the `Lsp` variant is `#[cfg]`'d
        // out, so `Command` is uninhabited and `Some(_)` can never be constructed — but
        // recognizing that as exhaustive (so a bare `None` arm suffices) needs
        // `min_exhaustive_patterns`, stabilized in Rust 1.82. This crate's MSRV is 1.80,
        // so match `_` here: it's exhaustive on 1.80/1.81 in both the `lsp` and
        // `cli`-only builds, and still runs the default lint for `None` (the only
        // reachable case when `lsp` is off, and the only *other* case when it's on).
        _ => run(cli.args),
    }
}

/// Everything the CLI front-end resolves from the args + config: the selected inputs,
/// the effective gating/format settings, and a per-file [`LintOptions`] builder. Both the
/// `pgsafe` binary and superset binaries (e.g. pgsafe-pro) build their run loop over this.
#[non_exhaustive]
#[derive(Debug)]
pub struct ResolvedRun {
    /// Selected inputs after `--git-diff` / `--since` / config selection: `(display-name, sql)`.
    pub inputs: Vec<(String, String)>,
    /// Effective gate threshold (CLI-explicit > config > built-in default).
    pub fail_on: FailOn,
    /// Effective output format (CLI-explicit > config > built-in default).
    pub format: Format,
    config: config::Config,
    config_dir: Option<PathBuf>,
    assume_in_transaction: bool,
}

impl ResolvedRun {
    /// The per-file lint options: `assume_in_transaction` + this file's config `disabled_rules`
    /// (path-relative) + the global `severity_overrides` and `enabled_rules`.
    #[must_use]
    pub fn options_for(&self, name: &str) -> LintOptions {
        config::options_from(
            &self.config,
            self.config_dir.as_deref(),
            name,
            self.assume_in_transaction,
        )
    }
}

/// Resolve config (discovery / `--config` / `--no-config`), select and read the inputs
/// (`--git-diff` / `--since` / positional paths, with all guards), and the effective scalar
/// settings. The `Err` is a human-readable message the caller prints as `error: {msg}` (exit 2).
///
/// # Errors
/// Returns a message when the config can't be loaded, an input can't be read, or a selection
/// guard fails (e.g. `-`/stdin combined with `--git-diff` or `--since`).
pub fn resolve(args: &CommonArgs) -> Result<ResolvedRun, String> {
    let (config, config_dir) = load_config(args)?;
    let inputs = select_inputs(args, &config)?;
    let inputs = scope_to_paths(inputs, &config, config_dir.as_deref());
    let fail_on = args.fail_on.or(config.fail_on).unwrap_or(FailOn::Warning);
    let format = args
        .format
        .clone()
        .or(config.format.clone())
        .unwrap_or(Format::Human);
    let assume_in_transaction = args.in_transaction || config.in_transaction.unwrap_or(false);
    Ok(ResolvedRun {
        inputs,
        fail_on,
        format,
        config,
        config_dir,
        assume_in_transaction,
    })
}

/// The annotated example configuration printed by `--example-config`. Exposed so superset CLIs
/// (e.g. pgsafe-pro, which shares this config) can offer the same flag.
#[must_use]
pub fn example_config() -> &'static str {
    config::EXAMPLE_CONFIG
}

/// Read, lint, render, and gate the inputs in `args`, returning the process
/// exit code (`0` clean, `1` gated findings, `2` parse/IO error).
#[must_use]
pub fn run(args: CommonArgs) -> ExitCode {
    if args.example_config {
        print!("{}", config::EXAMPLE_CONFIG);
        return ExitCode::SUCCESS;
    }
    if args.list_rules {
        let ids = crate::list_rule_ids();
        if matches!(args.format, Some(Format::Json)) {
            let envelope = serde_json::json!({ "schema_version": 1, "rules": ids });
            println!("{envelope}");
        } else {
            for id in ids {
                println!("{id}");
            }
        }
        return ExitCode::SUCCESS;
    }
    if args.fix || args.diff {
        let r = match resolve(&args) {
            Ok(r) => r,
            Err(msg) => {
                eprintln!("error: {msg}");
                return ExitCode::from(2);
            }
        };
        // Fix mode is human-output; reject a machine format whether it came from the
        // --format flag or the config file (check the resolved format, not just the flag).
        if matches!(r.format, Format::Json | Format::Github | Format::Sarif) {
            eprintln!(
                "error: --fix/--diff cannot be combined with --format json, github, or sarif"
            );
            return ExitCode::from(2);
        }
        let mode = if args.fix {
            fix::Mode::Apply
        } else {
            fix::Mode::Diff
        };
        return fix::run(&r, mode);
    }
    let r = match resolve(&args) {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::from(2);
        }
    };

    let reports: Vec<FileReport> = r
        .inputs
        .iter()
        .map(|(name, sql)| lint_input(name.clone(), sql, &r.options_for(name)))
        .collect();

    let had_error = reports.iter().any(|rep| rep.error.is_some());
    let had_findings = reports.iter().any(|rep| gate(&rep.findings, r.fail_on));

    match r.format {
        Format::Human => {
            let st = Styling::resolve(args.color);
            eprint!("{}", render_errors(&reports));
            print!("{}", render_human_styled(&reports, &st));
            if let Some(summary) = render_summary(&reports, &st) {
                println!("\n{summary}");
            }
        }
        Format::Json => match render_json(&reports) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        },
        Format::Github => print!("{}", render_github(&reports)),
        Format::Sarif => match render_sarif(&reports) {
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

/// Select + read the inputs per `--git-diff` / `--since` / positional paths.
fn select_inputs(
    args: &CommonArgs,
    config: &config::Config,
) -> Result<Vec<(String, String)>, String> {
    match &args.git_diff {
        Some(reference) => {
            if args.paths.iter().any(|p| p == "-") {
                return Err("`-` (stdin) cannot be combined with --git-diff".to_string());
            }
            let files = gitdiff::changed_sql_files(reference, &args.paths)?;
            read_files(&files)
        }
        None => {
            let effective_since = args.since.clone().or(config.since.clone());
            if effective_since.is_some() && args.paths.iter().any(|p| p == "-") {
                return Err(
                    "`-` (stdin) cannot be combined with a `since` cutoff (--since or the config file)"
                        .to_string(),
                );
            }
            match effective_since {
                Some(cutoff) if !args.paths.is_empty() => {
                    let kept: Vec<PathBuf> = filter_since(&args.paths, &cutoff)
                        .iter()
                        .map(PathBuf::from)
                        .collect();
                    read_files(&kept)
                }
                _ => read_inputs(&args.paths),
            }
        }
    }
}

/// Drop file inputs the config's `paths` globs don't govern. No-op when `paths`
/// is unset (`Config::in_scope` returns true for everything), so a config without
/// `paths` selects exactly as before. Stdin (`"<stdin>"`) has no path identity and
/// is always kept — piped SQL is never filtered. When one or more files are
/// dropped, a note is written to stderr so a scoped-out file isn't silently
/// treated as clean.
fn scope_to_paths(
    inputs: Vec<(String, String)>,
    config: &config::Config,
    config_dir: Option<&Path>,
) -> Vec<(String, String)> {
    let mut skipped = Vec::new();
    let kept: Vec<(String, String)> = inputs
        .into_iter()
        .filter(|(name, _)| {
            if name == "<stdin>" || config.in_scope(config_dir, name) {
                true
            } else {
                skipped.push(name.clone());
                false
            }
        })
        .collect();
    if !skipped.is_empty() {
        eprintln!(
            "note: skipped {} file(s) not matching the configured `paths`: {}",
            skipped.len(),
            skipped.join(", ")
        );
    }
    kept
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
