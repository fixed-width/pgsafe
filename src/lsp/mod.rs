//! `pgsafe lsp` — a synchronous stdio Language Server surfacing pgsafe findings
//! as editor diagnostics and quickfix code actions. Thin wrapper over `lint_sql`.

// `pgsafe lsp` (src/cli/mod.rs's `main_entry`) is the production caller of `run()`.
// This `allow` stays anyway: an `lsp`-only build (no `cli` feature) has no binary
// wiring anything up, so `cargo clippy --no-default-features --features lsp -- -D
// warnings` would otherwise flag this whole subtree as dead code. Lint levels cascade
// to child modules, so this one `allow` at the `lsp` module root covers `server::serve`
// too.
#![allow(dead_code)]

mod actions;
mod diagnostics;
mod fixall;
mod hover;
mod position;
pub(crate) mod server;

/// Boxed error for the serve loop: `lsp-server` transport, JSON, and IO errors
/// all convert into this via `?`.
pub(crate) type LspError = Box<dyn std::error::Error + Send + Sync>;

/// Run the language server over stdin/stdout until the client sends `shutdown`/`exit`.
///
/// # Errors
/// Returns any transport, protocol, or serialization error that aborts the session.
pub fn run() -> Result<(), LspError> {
    server::serve()
}
