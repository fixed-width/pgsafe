use std::io::Read;
use std::process::ExitCode;

use clap::Parser;
use pgsafe::{lint_sql, Finding};

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
}

#[derive(Clone, clap::ValueEnum)]
enum Format {
    Human,
    Json,
}

#[derive(serde::Serialize)]
struct FileReport {
    #[serde(rename = "file")]
    name: String,
    findings: Vec<Finding>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(true) => ExitCode::from(1),
        Ok(false) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> Result<bool, String> {
    let inputs = read_inputs(&cli.paths)?;
    let mut reports = Vec::new();
    for (name, sql) in inputs {
        let findings = lint_sql(&sql).map_err(|e| format!("{name}: {e}"))?;
        reports.push(FileReport { name, findings });
    }
    let any = reports.iter().any(|r| !r.findings.is_empty());
    match cli.format {
        Format::Human => print_human(&reports),
        Format::Json => print_json(&reports),
    }
    Ok(any)
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

fn print_json(reports: &[FileReport]) {
    let json = serde_json::to_string_pretty(reports).unwrap_or_else(|_| "[]".to_string());
    println!("{json}");
}

fn print_human(reports: &[FileReport]) {
    for r in reports {
        for f in &r.findings {
            println!(
                "{}: {} [{}] statement #{} (byte {})",
                r.name, f.severity, f.rule_id, f.statement_index, f.location
            );
            println!("  {}", f.message);
            println!("  fix: {}", f.guidance);
            if !f.snippet.is_empty() {
                println!("  | {}", f.snippet);
            }
        }
    }
}
