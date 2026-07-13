//! End-to-end LSP tests driven over lsp-server's in-memory connection.
#![cfg(feature = "lsp")]

use std::thread;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    DidOpenTextDocumentParams, InitializeParams, InitializedParams, PublishDiagnosticsParams,
    TextDocumentItem, Uri,
};

/// Spawn the server on one end of an in-memory connection; return the client end
/// plus the server thread handle.
fn start() -> (Connection, thread::JoinHandle<()>) {
    let (server, client) = Connection::memory();
    let handle = thread::spawn(move || {
        // The server initialize handshake consumes the `initialize` request and
        // `initialized` notification, then runs the loop until shutdown/exit.
        pgsafe::testing::lsp_run_loop_with_handshake(&server).unwrap();
    });
    (client, handle)
}

fn notify(conn: &Connection, method: &str, params: serde_json::Value) {
    conn.sender
        .send(Message::Notification(Notification {
            method: method.to_string(),
            params,
        }))
        .unwrap();
}

fn uri(s: &str) -> Uri {
    s.parse().unwrap()
}

#[test]
fn open_publishes_diagnostics() {
    let (client, handle) = start();

    // initialize handshake
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    // consume the initialize response
    let _ = client.receiver.recv().unwrap();
    notify(
        &client,
        "initialized",
        serde_json::to_value(InitializedParams {}).unwrap(),
    );

    // didOpen an unsafe migration
    let open = DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: uri("file:///tmp/0001_add_index.sql"),
            language_id: "sql".to_string(),
            version: 1,
            text: "CREATE INDEX idx ON t (col);".to_string(),
        },
    };
    notify(
        &client,
        "textDocument/didOpen",
        serde_json::to_value(open).unwrap(),
    );

    // expect a publishDiagnostics notification with at least one diagnostic
    let msg = client.receiver.recv().unwrap();
    let Message::Notification(n) = msg else {
        panic!("expected a notification, got {msg:?}");
    };
    assert_eq!(n.method, "textDocument/publishDiagnostics");
    let params: PublishDiagnosticsParams = serde_json::from_value(n.params).unwrap();
    assert!(!params.diagnostics.is_empty());

    // shutdown/exit so the server thread ends
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = client.receiver.recv().unwrap(); // shutdown response
    notify(&client, "exit", serde_json::Value::Null);
    handle.join().unwrap();
}
