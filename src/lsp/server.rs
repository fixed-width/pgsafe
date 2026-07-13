//! Connection handshake, document store, and the dispatch loop.

use std::collections::HashMap;
use std::path::PathBuf;

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
}

/// Real stdio serve loop.
pub(crate) fn serve() -> Result<(), LspError> {
    let (connection, io_threads) = Connection::stdio();
    handshake_and_run(&connection)?;
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
                            Some(path) => config::options_for_path(&path, false),
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
                    publish(connection, &doc)?;
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
                        publish(connection, doc)?;
                    }
                }
                "textDocument/didSave" => {
                    let p: DidSaveTextDocumentParams = serde_json::from_value(not.params)?;
                    let key = p.text_document.uri.as_str().to_string();
                    if let Some(doc) = state.docs.get(&key) {
                        publish(connection, doc)?;
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
fn publish(connection: &Connection, doc: &Document) -> Result<(), LspError> {
    let diagnostics = if doc.language_id == "sql" {
        let options = match uri_to_path(doc.uri.as_str()) {
            Some(path) => config::options_for_path(&path, false),
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
