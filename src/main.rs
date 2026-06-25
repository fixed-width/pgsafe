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
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
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
        match lint_sql(&sql) {
            Ok(findings) => {
                had_findings |= !findings.is_empty();
                reports.push(FileReport {
                    name,
                    findings,
                    error: None,
                });
            }
            Err(e) => {
                had_error = true;
                reports.push(FileReport {
                    name,
                    findings: Vec::new(),
                    error: Some(e.to_string()),
                });
            }
        }
    }

    match cli.format {
        Format::Human => print_human(&reports),
        Format::Json => print_json(&reports)?,
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
            println!(
                "{}: {} [{}] statement #{} (line {}, col {})",
                r.name,
                f.severity,
                f.rule_id,
                f.statement_index,
                f.location.line,
                f.location.column
            );
            println!("  {}", f.message);
            println!("  fix: {}", f.guidance);
            if !f.snippet.is_empty() {
                println!("  | {}", f.snippet);
            }
        }
    }
}
