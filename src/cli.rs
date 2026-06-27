//! Command-line surface (only built with the `cli` feature). A superset binary
//! can `#[command(flatten)]` [`CommonArgs`] and reuse [`run`] or its building
//! blocks instead of re-implementing argument parsing, rendering, and gating.

use std::io::Read;
use std::process::ExitCode;

use crate::{
    gate, lint_input, render_errors, render_human, render_json, FailOn, FileReport, Format,
};

/// The flags shared by every pgsafe-style CLI. Flatten this into a larger
/// `clap` parser to inherit them.
#[non_exhaustive]
#[derive(clap::Args)]
pub struct CommonArgs {
    /// SQL files to lint; use '-' or omit to read from stdin.
    pub paths: Vec<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    pub format: Format,
    /// Minimum finding severity that fails the run (exit code 1).
    #[arg(long, value_enum, default_value_t = FailOn::Warning)]
    pub fail_on: FailOn,
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
    let inputs = match read_inputs(&args.paths) {
        Ok(i) => i,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::from(2);
        }
    };

    let reports: Vec<FileReport> = inputs
        .into_iter()
        .map(|(name, sql)| lint_input(name, &sql, &crate::LintOptions::default()))
        .collect();

    let had_error = reports.iter().any(|r| r.error.is_some());
    let had_findings = reports.iter().any(|r| gate(&r.findings, args.fail_on));

    match args.format {
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

fn read_stdin() -> Result<String, String> {
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .map_err(|e| e.to_string())?;
    Ok(s)
}
