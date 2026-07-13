//! `pgsafe` binary — a thin wrapper over the library's CLI entry point.
use clap::Parser;

fn main() -> std::process::ExitCode {
    pgsafe::cli::main_entry(pgsafe::cli::Cli::parse())
}
