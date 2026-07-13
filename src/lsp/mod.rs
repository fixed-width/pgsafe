//! `pgsafe lsp` — a synchronous stdio Language Server surfacing pgsafe findings
//! as editor diagnostics and quickfix code actions. Thin wrapper over `lint_sql`.

// Scaffold-only: nothing calls `run()` yet (the CLI wiring lands in a later task), so
// `cargo clippy --features lsp -- -D warnings` would otherwise flag this whole subtree
// as dead code. Lint levels cascade to child modules, so this one `allow` at the `lsp`
// module root covers `server::serve` too. Remove once a caller reaches `run()`.
#![allow(dead_code)]

mod actions;
mod diagnostics;
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
