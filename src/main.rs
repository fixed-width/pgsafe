//! `pgsafe` binary — a thin wrapper over the library's CLI entry point.
use clap::Parser;

fn main() -> std::process::ExitCode {
    pgsafe::cli::run(pgsafe::cli::Cli::parse().args)
}
