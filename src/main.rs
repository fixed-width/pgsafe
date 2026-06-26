//! `pgsafe` binary — command-line interface to the pgsafe linter.
use std::io::Read;
use std::process::ExitCode;

use clap::Parser;
use pgsafe::{gate, lint_input, FailOn, FileReport, Format};

#[derive(Parser)]
#[command(
    name = "pgsafe",
    about = "Lint PostgreSQL DDL migrations for unsafe operations"
)]
struct Cli {
    /// SQL files to lint; use '-' or omit to read from stdin.
    paths: Vec<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    format: Format,
    /// Minimum finding severity that fails the run (exit code 1).
    #[arg(long, value_enum, default_value_t = FailOn::Warning)]
    fail_on: FailOn,
}

#[derive(serde::Serialize)]
struct Report<'a> {
    schema_version: u32,
    files: &'a [FileReport],
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> Result<u8, String> {
    let inputs = read_inputs(&cli.paths)?; // input/IO errors (e.g. missing file) still abort -> exit 2
    let mut reports = Vec::new();
    let mut had_error = false;
    let mut had_findings = false;

    for (name, sql) in inputs {
        let report = lint_input(name, &sql);
        had_error |= report.error.is_some();
        had_findings |= gate(&report.findings, cli.fail_on);
        reports.push(report);
    }

    match cli.format {
        Format::Human => print_human(&reports),
        Format::Json => print_json(&reports)?,
        _ => unreachable!("unknown format variant"),
    }

    Ok(if had_error {
        2
    } else if had_findings {
        1
    } else {
        0
    })
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

fn print_json(reports: &[FileReport]) -> Result<(), String> {
    let report = Report {
        schema_version: 1,
        files: reports,
    };
    let json = serde_json::to_string_pretty(&report)
        .map_err(|e| format!("failed to serialize JSON output: {e}"))?;
    println!("{json}");
    Ok(())
}

fn print_human(reports: &[FileReport]) {
    for r in reports {
        if let Some(err) = &r.error {
            eprintln!("{}: {}", r.name, err);
        }
        for f in &r.findings {
            let suffix = match &f.suppression {
                Some(s) => format!("  — suppressed: {}", s.reason),
                None => String::new(),
            };
            println!(
                "{}: {} [{}] statement #{} (line {}, col {}){}",
                r.name,
                f.severity,
                f.rule_id,
                f.statement_index,
                f.location.line,
                f.location.column,
                suffix
            );
            println!("  {}", f.message);
            if f.suppression.is_none() {
                println!("  fix: {}", f.guidance);
            }
            if !f.snippet.is_empty() {
                println!("  | {}", f.snippet);
            }
        }
    }
}
