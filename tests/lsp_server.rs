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

#[test]
fn code_action_returns_quickfix() {
    use lsp_types::{
        CodeActionContext, CodeActionParams, PartialResultParams, Position, Range,
        TextDocumentIdentifier, WorkDoneProgressParams,
    };

    let (client, handle) = start();

    // initialize
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let _ = client.receiver.recv().unwrap();
    notify(
        &client,
        "initialized",
        serde_json::to_value(InitializedParams {}).unwrap(),
    );

    let doc_uri = uri("file:///tmp/0001.sql");
    let open = DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: doc_uri.clone(),
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
    let _ = client.receiver.recv().unwrap(); // publishDiagnostics

    let ca = CodeActionParams {
        text_document: TextDocumentIdentifier {
            uri: doc_uri.clone(),
        },
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 27,
            },
        },
        context: CodeActionContext::default(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/codeAction".to_string(),
            params: serde_json::to_value(ca).unwrap(),
        }))
        .unwrap();

    let resp = loop {
        match client.receiver.recv().unwrap() {
            Message::Response(r) if r.id == RequestId::from(2) => break r,
            _ => continue,
        }
    };
    let value = match resp.response_kind {
        lsp_server::ResponseKind::Ok { result } => result,
        other => panic!("expected an Ok code-action response, got {other:?}"),
    };
    let arr = value.as_array().expect("array of actions");
    assert!(!arr.is_empty(), "expected at least one quickfix");

    // shutdown/exit
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = client.receiver.recv().unwrap();
    notify(&client, "exit", serde_json::Value::Null);
    handle.join().unwrap();
}

/// End-to-end proof of `paths`-scoped linting: a
/// `.pgsafe.toml` with `paths = ["migrations/**"]` scopes the server to the
/// migrations directory. A doc opened under `migrations/` gets diagnostics for
/// unsafe DDL; the identical DDL opened under a sibling directory gets none. Neither
/// subdirectory needs to exist on disk — only the `.pgsafe.toml` does, for discovery;
/// linting works on the in-memory document text.
#[test]
fn paths_scoping_gates_diagnostics_to_matching_files() {
    let (client, handle) = start();

    // initialize
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let _ = client.receiver.recv().unwrap();
    notify(
        &client,
        "initialized",
        serde_json::to_value(InitializedParams {}).unwrap(),
    );

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".pgsafe.toml"),
        "paths = [\"migrations/**\"]\n",
    )
    .unwrap();

    // In-scope doc: under migrations/.
    let in_scope_path = dir.path().join("migrations").join("0001.sql");
    let in_scope_uri = uri(&format!("file://{}", in_scope_path.display()));
    notify(
        &client,
        "textDocument/didOpen",
        serde_json::to_value(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: in_scope_uri.clone(),
                language_id: "sql".to_string(),
                version: 1,
                text: "CREATE INDEX idx ON t (col);".to_string(),
            },
        })
        .unwrap(),
    );
    let msg = client.receiver.recv().unwrap();
    let Message::Notification(n) = msg else {
        panic!("expected a notification, got {msg:?}");
    };
    assert_eq!(n.method, "textDocument/publishDiagnostics");
    let params: PublishDiagnosticsParams = serde_json::from_value(n.params).unwrap();
    assert_eq!(params.uri, in_scope_uri);
    assert!(
        !params.diagnostics.is_empty(),
        "expected diagnostics for a file under migrations/"
    );

    // Out-of-scope doc: identical unsafe DDL, sibling directory.
    let out_of_scope_path = dir.path().join("queries").join("q.sql");
    let out_of_scope_uri = uri(&format!("file://{}", out_of_scope_path.display()));
    notify(
        &client,
        "textDocument/didOpen",
        serde_json::to_value(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: out_of_scope_uri.clone(),
                language_id: "sql".to_string(),
                version: 1,
                text: "CREATE INDEX idx ON t (col);".to_string(),
            },
        })
        .unwrap(),
    );
    let msg = client.receiver.recv().unwrap();
    let Message::Notification(n) = msg else {
        panic!("expected a notification, got {msg:?}");
    };
    assert_eq!(n.method, "textDocument/publishDiagnostics");
    let params: PublishDiagnosticsParams = serde_json::from_value(n.params).unwrap();
    assert_eq!(params.uri, out_of_scope_uri);
    assert!(
        params.diagnostics.is_empty(),
        "expected no diagnostics for a file outside `paths` scope, got {:?}",
        params.diagnostics
    );

    // shutdown/exit
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = client.receiver.recv().unwrap();
    notify(&client, "exit", serde_json::Value::Null);
    handle.join().unwrap();
}

/// End-to-end proof of the config-cache invalidation wiring (not just the isolated
/// `ConfigCache` unit test): open a SQL doc whose directory has no config (the
/// default-enabled rule fires), then write a `.pgsafe.toml` disabling that rule and
/// `didSave` it — even though the config file itself was never `didOpen`ed. The
/// server must invalidate its cache and re-publish the open SQL doc unprompted, with
/// no further `didChange` needed.
#[test]
fn did_save_of_config_file_invalidates_cache_and_relints() {
    use lsp_types::{DidSaveTextDocumentParams, TextDocumentIdentifier};

    let (client, handle) = start();

    // initialize
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let _ = client.receiver.recv().unwrap();
    notify(
        &client,
        "initialized",
        serde_json::to_value(InitializedParams {}).unwrap(),
    );

    let dir = tempfile::tempdir().unwrap();
    let sql_path = dir.path().join("0001_add_index.sql");
    let sql_uri = uri(&format!("file://{}", sql_path.display()));

    let open = DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: sql_uri.clone(),
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

    // Initial publish: no config on disk yet, so add-index-non-concurrent (on by
    // default) fires.
    let msg = client.receiver.recv().unwrap();
    let Message::Notification(n) = msg else {
        panic!("expected a notification, got {msg:?}");
    };
    let params: PublishDiagnosticsParams = serde_json::from_value(n.params).unwrap();
    assert!(
        !params.diagnostics.is_empty(),
        "expected findings before the config disables the rule"
    );

    // Write a config disabling the rule, then didSave it (it was never didOpen'd).
    let cfg_path = dir.path().join(".pgsafe.toml");
    std::fs::write(&cfg_path, "[rules]\nadd-index-non-concurrent = false\n").unwrap();
    let cfg_uri = uri(&format!("file://{}", cfg_path.display()));
    notify(
        &client,
        "textDocument/didSave",
        serde_json::to_value(DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri: cfg_uri },
            text: None,
        })
        .unwrap(),
    );

    // The server re-lints the open SQL doc under the same directory unprompted; the
    // now-disabled rule's diagnostic is gone (a still-enabled rule, require-timeout,
    // is expected to remain — this asserts the specific rule cleared, not silence).
    let msg = client.receiver.recv().unwrap();
    let Message::Notification(n) = msg else {
        panic!("expected a notification, got {msg:?}");
    };
    assert_eq!(n.method, "textDocument/publishDiagnostics");
    let params: PublishDiagnosticsParams = serde_json::from_value(n.params).unwrap();
    assert_eq!(params.uri, sql_uri);
    assert!(
        !params.diagnostics.iter().any(|d| d.code
            == Some(lsp_types::NumberOrString::String(
                "add-index-non-concurrent".to_string()
            ))),
        "expected add-index-non-concurrent to be disabled after the config was saved, got {:?}",
        params.diagnostics
    );

    // shutdown/exit
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = client.receiver.recv().unwrap();
    notify(&client, "exit", serde_json::Value::Null);
    handle.join().unwrap();
}
