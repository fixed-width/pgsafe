//! Connection handshake, document store, and the dispatch loop.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_server::{Connection, Message, Response};
use lsp_types::{
    CodeActionKind, CodeActionOptions, CodeActionParams, CodeActionProviderCapability,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, PositionEncodingKind, PublishDiagnosticsParams, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};

use super::diagnostics::diagnostics_for;
use super::LspError;
use crate::{config, lint_sql};

/// One open document the server is tracking.
struct Document {
    uri: Uri,
    language_id: String,
    text: String,
}

/// Server state carried across messages.
#[derive(Default)]
struct State {
    docs: HashMap<String, Document>,
    configs: ConfigCache,
}

/// The basenames pgsafe treats as a project config file (matches `config::discover`).
const CONFIG_BASENAMES: [&str; 2] = ["pgsafe.toml", ".pgsafe.toml"];

/// Caches resolved `LintOptions` keyed by the document's parent directory, so a
/// keystroke doesn't re-read `.pgsafe.toml` from disk each time. Invalidated when
/// a `.pgsafe.toml` under that directory is saved.
#[derive(Default)]
pub(crate) struct ConfigCache {
    by_dir: HashMap<PathBuf, crate::LintOptions>,
}

impl ConfigCache {
    /// Options for `file_path`, resolving (and caching) on first use per directory.
    pub(crate) fn options_for(&mut self, file_path: &Path, in_txn: bool) -> crate::LintOptions {
        let dir = file_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        self.by_dir
            .entry(dir)
            .or_insert_with(|| config::options_for_path(file_path, in_txn))
            .clone()
    }

    /// Drop cached entries at or under `dir` (called when a `.pgsafe.toml` saves).
    pub(crate) fn invalidate_dir(&mut self, dir: &Path) {
        self.by_dir.retain(|k, _| !k.starts_with(dir));
    }
}

/// Real stdio serve loop.
pub(crate) fn serve() -> Result<(), LspError> {
    let (connection, io_threads) = Connection::stdio();
    handshake_and_run(&connection)?;
    // Drop `connection` (and with it, `connection.sender`) before joining: the
    // background writer thread only stops once every `Sender<Message>` clone —
    // including this one — is gone, since that's what ends its channel iterator.
    // Holding `connection` alive across `join()` (e.g. by inlining these two calls
    // without the explicit drop) deadlocks forever, because the writer is then
    // waiting on a sender that only goes away after the join it's blocking.
    drop(connection);
    io_threads.join()?;
    Ok(())
}

/// Perform the initialize handshake advertising our capabilities, then dispatch.
/// Exposed to the integration suite via `crate::testing`.
pub(crate) fn handshake_and_run(connection: &Connection) -> Result<(), LspError> {
    let caps = serde_json::to_value(server_capabilities())?;
    let _init_params = connection.initialize(caps)?;
    run_loop(connection)
}

/// The advertised server capabilities.
pub(crate) fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
            ..CodeActionOptions::default()
        })),
        ..ServerCapabilities::default()
    }
}

/// Dispatch messages until `shutdown`/`exit`.
pub(crate) fn run_loop(connection: &Connection) -> Result<(), LspError> {
    let mut state = State::default();
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                if req.method == "textDocument/codeAction" {
                    let (id, params) = req
                        .extract::<CodeActionParams>("textDocument/codeAction")
                        .map_err(|e| format!("codeAction params: {e:?}"))?;
                    let key = params.text_document.uri.as_str().to_string();
                    let result = if let Some(doc) = state.docs.get(&key) {
                        let options = match uri_to_path(&key) {
                            Some(path) => state.configs.options_for(&path, false),
                            None => crate::LintOptions::default(),
                        };
                        let findings = lint_sql(&doc.text, &options).unwrap_or_default();
                        super::actions::code_actions(&doc.uri, &doc.text, &findings, params.range)
                    } else {
                        Vec::new()
                    };
                    connection.sender.send(Message::Response(Response {
                        id,
                        result: Some(serde_json::to_value(result)?),
                        error: None,
                    }))?;
                }
                // Other requests are ignored for MVP.
            }
            Message::Notification(not) => match not.method.as_str() {
                "exit" => return Ok(()),
                "textDocument/didOpen" => {
                    let p: DidOpenTextDocumentParams = serde_json::from_value(not.params)?;
                    let doc = Document {
                        uri: p.text_document.uri,
                        language_id: p.text_document.language_id,
                        text: p.text_document.text,
                    };
                    let key = doc.uri.as_str().to_string();
                    publish(connection, &doc, &mut state.configs)?;
                    state.docs.insert(key, doc);
                }
                "textDocument/didChange" => {
                    let p: DidChangeTextDocumentParams = serde_json::from_value(not.params)?;
                    let key = p.text_document.uri.as_str().to_string();
                    if let Some(doc) = state.docs.get_mut(&key) {
                        // FULL sync: the last change contains the whole document.
                        if let Some(change) = p.content_changes.into_iter().last() {
                            doc.text = change.text;
                        }
                        let doc = state.docs.get(&key).expect("just updated");
                        publish(connection, doc, &mut state.configs)?;
                    }
                }
                "textDocument/didSave" => {
                    let p: DidSaveTextDocumentParams = serde_json::from_value(not.params)?;
                    let uri = p.text_document.uri;
                    let saved_path = uri_to_path(uri.as_str());
                    let config_dir = saved_path.as_deref().filter(|p| is_config_file(p));

                    if let Some(dir) = config_dir.and_then(Path::parent) {
                        // A `.pgsafe.toml`/`pgsafe.toml` saved: its directory (and any
                        // subdirectory) may now resolve to different options, so drop the
                        // stale cache entries and re-lint every open SQL doc under it —
                        // not just the config file itself, which usually isn't a tracked
                        // document at all.
                        state.configs.invalidate_dir(dir);
                        let affected: Vec<String> = state
                            .docs
                            .iter()
                            .filter(|(_, doc)| {
                                doc.language_id == "sql"
                                    && uri_to_path(doc.uri.as_str())
                                        .and_then(|p| p.parent().map(Path::to_path_buf))
                                        .is_some_and(|d| d.starts_with(dir))
                            })
                            .map(|(key, _)| key.clone())
                            .collect();
                        for key in affected {
                            if let Some(doc) = state.docs.get(&key) {
                                publish(connection, doc, &mut state.configs)?;
                            }
                        }
                    } else {
                        let key = uri.as_str().to_string();
                        if let Some(doc) = state.docs.get(&key) {
                            publish(connection, doc, &mut state.configs)?;
                        }
                    }
                }
                "textDocument/didClose" => {
                    let p: DidCloseTextDocumentParams = serde_json::from_value(not.params)?;
                    let key = p.text_document.uri.as_str().to_string();
                    if let Some(doc) = state.docs.remove(&key) {
                        // Clear diagnostics for the closed document.
                        clear(connection, &doc.uri)?;
                    }
                }
                _ => {}
            },
            Message::Response(_) => {}
        }
    }
    Ok(())
}

/// Lint a document and publish its diagnostics (empty on parse error / non-SQL).
/// Takes the config cache separately (not via `&State`) so callers can hold an
/// immutable borrow of `state.docs` (for `doc`) alongside this mutable one — the two
/// borrows are of disjoint `State` fields, so the split is enough to satisfy the
/// borrow checker without cloning the document.
fn publish(
    connection: &Connection,
    doc: &Document,
    configs: &mut ConfigCache,
) -> Result<(), LspError> {
    let diagnostics = if doc.language_id == "sql" {
        let options = match uri_to_path(doc.uri.as_str()) {
            Some(path) => configs.options_for(&path, false),
            None => crate::LintOptions::default(),
        };
        match lint_sql(&doc.text, &options) {
            Ok(findings) => diagnostics_for(&doc.text, &findings),
            Err(_) => Vec::new(), // mid-edit / unparseable: clear, don't spam
        }
    } else {
        Vec::new()
    };
    send_diagnostics(connection, &doc.uri, diagnostics)
}

fn clear(connection: &Connection, uri: &Uri) -> Result<(), LspError> {
    send_diagnostics(connection, uri, Vec::new())
}

fn send_diagnostics(
    connection: &Connection,
    uri: &Uri,
    diagnostics: Vec<lsp_types::Diagnostic>,
) -> Result<(), LspError> {
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics,
        version: None,
    };
    connection
        .sender
        .send(Message::Notification(lsp_server::Notification {
            method: "textDocument/publishDiagnostics".to_string(),
            params: serde_json::to_value(params)?,
        }))?;
    Ok(())
}

/// Whether `path`'s file name is a pgsafe config file (`pgsafe.toml` / `.pgsafe.toml`),
/// matching `config::discover`'s candidates.
fn is_config_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| CONFIG_BASENAMES.contains(&name))
}

/// Convert a `file://` URI string to a filesystem path (percent-decoded). Returns
/// `None` for non-`file` schemes or unparseable input, in which case the caller
/// falls back to default lint options.
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Strip an optional authority (e.g. "localhost") before the path.
    let path_part = match rest.find('/') {
        Some(0) => rest,       // "file:///path" — authority empty
        Some(i) => &rest[i..], // "file://host/path"
        None => return None,
    };
    Some(PathBuf::from(percent_decode(path_part)))
}

/// Minimal `%XX` percent-decoding, sufficient for file URIs (e.g. `%20` → space).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod cache_tests {
    use super::ConfigCache;
    use std::io::Write;

    #[test]
    fn caches_then_invalidates() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".pgsafe.toml");
        let mut f = std::fs::File::create(&cfg).unwrap();
        writeln!(f, "[rules]\nadd-index-non-concurrent = false").unwrap();
        let sql = dir.path().join("a.sql");

        let mut cache = ConfigCache::default();
        let o1 = cache.options_for(&sql, false);
        assert!(o1.disabled_rules.contains("add-index-non-concurrent"));

        // Rewrite the config to re-enable the rule.
        std::fs::write(&cfg, "[rules]\n").unwrap();
        // Stale until invalidated.
        let o2 = cache.options_for(&sql, false);
        assert!(o2.disabled_rules.contains("add-index-non-concurrent"));
        // After invalidation, re-read reflects the new config.
        cache.invalidate_dir(dir.path());
        let o3 = cache.options_for(&sql, false);
        assert!(!o3.disabled_rules.contains("add-index-non-concurrent"));
    }
}

#[cfg(test)]
mod tests {
    use super::uri_to_path;
    use std::path::PathBuf;

    #[test]
    fn plain_file_uri() {
        assert_eq!(
            uri_to_path("file:///tmp/a.sql"),
            Some(PathBuf::from("/tmp/a.sql"))
        );
    }

    #[test]
    fn percent_encoded_space() {
        assert_eq!(
            uri_to_path("file:///tmp/my%20dir/a.sql"),
            Some(PathBuf::from("/tmp/my dir/a.sql"))
        );
    }

    #[test]
    fn non_file_scheme_is_none() {
        assert_eq!(uri_to_path("untitled:Untitled-1"), None);
    }
}
