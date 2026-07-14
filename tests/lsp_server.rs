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

/// Drive one `textDocument/codeAction` request with an explicit `context.only`
/// filter over a freshly-opened unsafe migration, returning the response's JSON
/// result array. Keeps the two `source.fixAll` routing tests below focused on the
/// filtering behavior rather than the handshake boilerplate.
fn code_action_kinds_for_only(only: &[&str]) -> Vec<serde_json::Value> {
    use lsp_types::{
        CodeActionContext, CodeActionKind, CodeActionParams, PartialResultParams, Position, Range,
        TextDocumentIdentifier, WorkDoneProgressParams,
    };

    let (client, handle) = start();
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
    notify(
        &client,
        "textDocument/didOpen",
        serde_json::to_value(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "sql".to_string(),
                version: 1,
                text: "CREATE INDEX idx ON t (col);".to_string(),
            },
        })
        .unwrap(),
    );
    let _ = client.receiver.recv().unwrap(); // publishDiagnostics

    let ca = CodeActionParams {
        text_document: TextDocumentIdentifier { uri: doc_uri },
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
        context: CodeActionContext {
            only: Some(
                only.iter()
                    .map(|k| CodeActionKind::from(k.to_string()))
                    .collect(),
            ),
            ..CodeActionContext::default()
        },
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
    let actions = value.as_array().expect("array of actions").clone();

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
    actions
}

#[test]
fn source_fix_all_returns_a_whole_document_edit() {
    let actions = code_action_kinds_for_only(&["source.fixAll"]);
    // Only source.fixAll was requested → exactly the fix-all action, no quickfixes.
    assert!(
        actions.iter().all(|a| a["kind"] == "source.fixAll"),
        "expected only source.fixAll actions, got {actions:?}"
    );
    let fix_all = actions
        .iter()
        .find(|a| a["kind"] == "source.fixAll")
        .expect("a source.fixAll action");
    // Its edit is a single whole-document replacement carrying the fixed SQL.
    let edits = fix_all["edit"]["changes"]
        .as_object()
        .expect("changes map")
        .values()
        .next()
        .and_then(|v| v.as_array())
        .expect("edits array");
    assert_eq!(edits.len(), 1, "fix-all is one whole-document edit");
    let new_text = edits[0]["newText"].as_str().unwrap();
    assert!(
        new_text.contains("CONCURRENTLY"),
        "fixed text should carry the applied fix, got {new_text:?}"
    );
}

#[test]
fn quickfix_only_excludes_source_fix_all() {
    let actions = code_action_kinds_for_only(&["quickfix"]);
    assert!(
        !actions.is_empty(),
        "expected at least one quickfix for the unsafe statement"
    );
    assert!(
        actions.iter().all(|a| a["kind"] == "quickfix"),
        "quickfix-only request must not return source.fixAll, got {actions:?}"
    );
}

#[test]
fn parent_source_kind_covers_fix_all() {
    // A `source` umbrella request (a dotted parent of `source.fixAll`) must surface the
    // fix-all action but not range quickfixes — the on-save `source` code-action pass.
    let actions = code_action_kinds_for_only(&["source"]);
    assert!(
        actions.iter().any(|a| a["kind"] == "source.fixAll"),
        "a `source` request should cover source.fixAll, got {actions:?}"
    );
    assert!(
        actions.iter().all(|a| a["kind"] != "quickfix"),
        "a `source` request must not return range quickfixes, got {actions:?}"
    );
}

#[test]
fn hover_returns_finding_details() {
    use lsp_types::{
        Hover, HoverContents, HoverParams, Position, TextDocumentIdentifier,
        TextDocumentPositionParams, WorkDoneProgressParams,
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
    notify(
        &client,
        "textDocument/didOpen",
        serde_json::to_value(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "sql".to_string(),
                version: 1,
                text: "CREATE INDEX idx ON t (col);".to_string(),
            },
        })
        .unwrap(),
    );
    let _ = client.receiver.recv().unwrap(); // publishDiagnostics

    // Hover inside the flagged statement.
    let hp = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: doc_uri.clone(),
            },
            position: Position {
                line: 0,
                character: 3,
            },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/hover".to_string(),
            params: serde_json::to_value(hp).unwrap(),
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
        other => panic!("expected an Ok hover response, got {other:?}"),
    };
    let hover: Hover = serde_json::from_value(value).expect("a Hover result");
    let text = match hover.contents {
        HoverContents::Markup(m) => m.value,
        other => panic!("expected markup contents, got {other:?}"),
    };
    assert!(
        text.contains("add-index-non-concurrent"),
        "hover should name the rule, got: {text}"
    );
    assert!(
        hover.range.is_some(),
        "hover should carry the statement range"
    );

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

#[test]
fn hover_off_any_finding_returns_null() {
    use lsp_types::{
        HoverParams, Position, TextDocumentIdentifier, TextDocumentPositionParams,
        WorkDoneProgressParams,
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

    // The doc HAS a finding (on line 0); we hover far past it to prove position
    // gating — a null result here is because the cursor is off the finding, not
    // because the document is clean.
    let doc_uri = uri("file:///tmp/0001.sql");
    notify(
        &client,
        "textDocument/didOpen",
        serde_json::to_value(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "sql".to_string(),
                version: 1,
                text: "CREATE INDEX idx ON t (col);".to_string(),
            },
        })
        .unwrap(),
    );
    let _ = client.receiver.recv().unwrap(); // publishDiagnostics

    let hp = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: doc_uri.clone(),
            },
            position: Position {
                line: 5,
                character: 0,
            },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/hover".to_string(),
            params: serde_json::to_value(hp).unwrap(),
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
        other => panic!("expected an Ok hover response, got {other:?}"),
    };
    assert!(
        value.is_null(),
        "hovering off any finding should return JSON null, got {value:?}"
    );

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
/// `pgsafe.toml` with `paths = ["migrations/**"]` scopes the server to the
/// migrations directory. A doc opened under `migrations/` gets diagnostics for
/// unsafe DDL; the identical DDL opened under a sibling directory gets none. Neither
/// subdirectory needs to exist on disk — only the `pgsafe.toml` does, for discovery;
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
        dir.path().join("pgsafe.toml"),
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
/// default-enabled rule fires), then write a `pgsafe.toml` disabling that rule and
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
    let cfg_path = dir.path().join("pgsafe.toml");
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

#[test]
fn malformed_request_gets_error_response_and_server_survives() {
    // A request whose params don't deserialize must draw a JSON-RPC error response, not
    // tear the server down: the session has to survive one bad message.
    let (client, handle) = start();
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

    // codeAction with params that are not a CodeActionParams object.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/codeAction".to_string(),
            params: serde_json::json!("not a CodeActionParams object"),
        }))
        .unwrap();
    let resp = loop {
        match client.receiver.recv().unwrap() {
            Message::Response(r) if r.id == RequestId::from(2) => break r,
            _ => continue,
        }
    };
    match resp.response_kind {
        lsp_server::ResponseKind::Err { error } => assert_eq!(
            error.code, -32602,
            "malformed params should yield InvalidParams (-32602), got {error:?}"
        ),
        other => panic!("expected an error response, got {other:?}"),
    }

    // The server survived: a normal shutdown still round-trips and the thread joins clean.
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

#[test]
fn malformed_notification_is_ignored_and_server_survives() {
    // A notification whose params don't deserialize must be ignored (JSON-RPC has no
    // response for notifications), never fatal.
    let (client, handle) = start();
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

    // A didOpen with garbage params — must be dropped, not tear the server down.
    notify(
        &client,
        "textDocument/didOpen",
        serde_json::json!("garbage"),
    );

    // Liveness proof: shutdown still round-trips and the thread joins clean.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let resp = loop {
        match client.receiver.recv().unwrap() {
            Message::Response(r) if r.id == RequestId::from(2) => break r,
            _ => continue,
        }
    };
    assert!(matches!(
        resp.response_kind,
        lsp_server::ResponseKind::Ok { .. }
    ));
    notify(&client, "exit", serde_json::Value::Null);
    handle.join().unwrap();
}
