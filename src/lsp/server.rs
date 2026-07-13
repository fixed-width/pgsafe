//! Connection handshake, document store, and the dispatch loop.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use lsp_server::{Connection, Message, Notification, Request, Response};
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

/// Caches the *expensive* part of config resolution — discovery, file read, TOML
/// parse, and glob compilation — keyed by the document's parent directory, so a
/// keystroke doesn't redo that work each time. Invalidated when a `.pgsafe.toml`
/// under that directory is saved.
///
/// Deliberately does **not** cache the resolved [`crate::LintOptions`] itself:
/// `disabled_rules` is per-*file*, not per-directory (`config::options_from` unions
/// the global disables with every `[[ignore]]` glob that matches this specific file's
/// relative path). Caching the fully-resolved options by directory would return the
/// first file's `disabled_rules` for every sibling — an order-dependent false
/// negative if a later sibling file should have been ignored (or not) differently.
/// So `options_for` recomputes `disabled_rules` from the cached, already-compiled
/// `Config` on every call.
#[derive(Default)]
pub(crate) struct ConfigCache {
    by_dir: HashMap<PathBuf, (config::Config, Option<PathBuf>)>,
}

impl ConfigCache {
    /// Options for `file_path`: resolves (and caches, by directory) the underlying
    /// config on first use, then recomputes this file's `disabled_rules` from it on
    /// every call — so two sibling files with different `[[ignore]]` matches each get
    /// their own correct answer even though they share one cached `Config`.
    pub(crate) fn options_for(&mut self, file_path: &Path) -> crate::LintOptions {
        let dir = file_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let entry = self
            .by_dir
            .entry(dir)
            .or_insert_with(|| config::resolve_config(file_path));
        // The LSP has no per-invocation transaction flag to OR in (unlike the CLI's
        // `--in-transaction`), so it comes purely from the resolved `.pgsafe.toml`.
        let in_txn = entry.0.in_transaction.unwrap_or(false);
        config::options_from(
            &entry.0,
            entry.1.as_deref(),
            &file_path.to_string_lossy(),
            in_txn,
        )
    }

    /// Drop cached entries at or under `dir` (called when a `.pgsafe.toml` saves).
    pub(crate) fn invalidate_dir(&mut self, dir: &Path) {
        self.by_dir.retain(|k, _| !k.starts_with(dir));
    }

    /// Whether `file_path` is in scope of the resolved config's `paths` globs (§10 of
    /// the LSP design doc) and should be linted at all. Resolves (and caches, by
    /// directory) the config the same way `options_for` does, then delegates the
    /// unset-default and glob matching to `config::Config::in_scope` — the single
    /// place that logic lives.
    pub(crate) fn should_lint(&mut self, file_path: &Path) -> bool {
        let dir = file_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let entry = self
            .by_dir
            .entry(dir)
            .or_insert_with(|| config::resolve_config(file_path));
        entry
            .0
            .in_scope(entry.1.as_deref(), &file_path.to_string_lossy())
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
        let flow = match msg {
            Message::Request(req) => handle_request(connection, &mut state, req)?,
            Message::Notification(not) => handle_notification(connection, &mut state, not)?,
            Message::Response(_) => ControlFlow::Continue(()),
        };
        if flow.is_break() {
            return Ok(());
        }
    }
    Ok(())
}

/// Handle one client request: `shutdown` ends the loop; `textDocument/codeAction` is
/// answered; anything else is ignored for MVP.
fn handle_request(
    connection: &Connection,
    state: &mut State,
    req: Request,
) -> Result<ControlFlow<()>, LspError> {
    if connection.handle_shutdown(&req)? {
        return Ok(ControlFlow::Break(()));
    }
    if req.method == "textDocument/codeAction" {
        on_code_action(connection, state, req)?;
    }
    // Other requests are ignored for MVP.
    Ok(ControlFlow::Continue(()))
}

/// Handle one client notification: `exit` ends the loop; the `textDocument/*` sync
/// events update document/config state; anything else is ignored.
fn handle_notification(
    connection: &Connection,
    state: &mut State,
    not: Notification,
) -> Result<ControlFlow<()>, LspError> {
    match not.method.as_str() {
        "exit" => return Ok(ControlFlow::Break(())),
        "textDocument/didOpen" => on_did_open(connection, state, not.params)?,
        "textDocument/didChange" => on_did_change(connection, state, not.params)?,
        "textDocument/didSave" => on_did_save(connection, state, not.params)?,
        "textDocument/didClose" => on_did_close(connection, state, not.params)?,
        _ => {}
    }
    Ok(ControlFlow::Continue(()))
}

/// Handle `textDocument/codeAction`: lint the open document and respond with the
/// quick-fix actions that apply within the requested range.
fn on_code_action(
    connection: &Connection,
    state: &mut State,
    req: Request,
) -> Result<(), LspError> {
    let (id, params) = req
        .extract::<CodeActionParams>("textDocument/codeAction")
        .map_err(|e| format!("codeAction params: {e:?}"))?;
    let key = params.text_document.uri.as_str().to_string();
    let result = if let Some(doc) = state.docs.get(&key) {
        match resolve_lint_options(doc, &mut state.configs) {
            Some(options) => {
                let findings = lint_sql(&doc.text, &options).unwrap_or_default();
                super::actions::code_actions(&doc.uri, &doc.text, &findings, params.range)
            }
            None => Vec::new(), // out of `paths` scope: no quickfixes
        }
    } else {
        Vec::new()
    };
    connection.sender.send(Message::Response(Response {
        id,
        result: Some(serde_json::to_value(result)?),
        error: None,
    }))?;
    Ok(())
}

/// Handle `textDocument/didOpen`: publish the document's diagnostics, then start
/// tracking it (publish-before-insert order is intentional).
fn on_did_open(
    connection: &Connection,
    state: &mut State,
    params: serde_json::Value,
) -> Result<(), LspError> {
    let p: DidOpenTextDocumentParams = serde_json::from_value(params)?;
    let doc = Document {
        uri: p.text_document.uri,
        language_id: p.text_document.language_id,
        text: p.text_document.text,
    };
    let key = doc.uri.as_str().to_string();
    publish(connection, &doc, &mut state.configs)?;
    state.docs.insert(key, doc);
    Ok(())
}

/// Handle `textDocument/didChange`: apply the full-sync edit and republish
/// diagnostics.
fn on_did_change(
    connection: &Connection,
    state: &mut State,
    params: serde_json::Value,
) -> Result<(), LspError> {
    let p: DidChangeTextDocumentParams = serde_json::from_value(params)?;
    let key = p.text_document.uri.as_str().to_string();
    if let Some(doc) = state.docs.get_mut(&key) {
        // FULL sync: the last change contains the whole document.
        if let Some(change) = p.content_changes.into_iter().last() {
            doc.text = change.text;
        }
        let doc = state.docs.get(&key).expect("just updated");
        publish(connection, doc, &mut state.configs)?;
    }
    Ok(())
}

/// Handle `textDocument/didSave`: if a pgsafe config file saved, re-lint every open
/// SQL document under its directory; otherwise just republish the saved document's
/// diagnostics.
fn on_did_save(
    connection: &Connection,
    state: &mut State,
    params: serde_json::Value,
) -> Result<(), LspError> {
    let p: DidSaveTextDocumentParams = serde_json::from_value(params)?;
    let uri = p.text_document.uri;
    let saved_path = uri_to_path(uri.as_str());
    let config_dir = saved_path.as_deref().filter(|p| is_config_file(p));

    if let Some(dir) = config_dir.and_then(Path::parent) {
        // A `.pgsafe.toml`/`pgsafe.toml` saved: its directory (and any subdirectory)
        // may now resolve to different options, so drop the stale cache entries and
        // re-lint every open SQL doc under it — not just the config file itself,
        // which usually isn't a tracked document at all.
        relint_dir_after_config_save(connection, state, dir)?;
    } else {
        let key = uri.as_str().to_string();
        republish(connection, state, &key)?;
    }
    Ok(())
}

/// Drop `ConfigCache` entries at or under `dir` and republish diagnostics for every
/// open SQL document under it, since a config file saved there may have changed
/// their resolved lint options.
fn relint_dir_after_config_save(
    connection: &Connection,
    state: &mut State,
    dir: &Path,
) -> Result<(), LspError> {
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
        republish(connection, state, &key)?;
    }
    Ok(())
}

/// Republish diagnostics for the tracked document `key`, if it's still open.
fn republish(connection: &Connection, state: &mut State, key: &str) -> Result<(), LspError> {
    if let Some(doc) = state.docs.get(key) {
        publish(connection, doc, &mut state.configs)?;
    }
    Ok(())
}

/// Handle `textDocument/didClose`: stop tracking the document and clear its
/// diagnostics.
fn on_did_close(
    connection: &Connection,
    state: &mut State,
    params: serde_json::Value,
) -> Result<(), LspError> {
    let p: DidCloseTextDocumentParams = serde_json::from_value(params)?;
    let key = p.text_document.uri.as_str().to_string();
    if let Some(doc) = state.docs.remove(&key) {
        // Clear diagnostics for the closed document.
        clear(connection, &doc.uri)?;
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
        match resolve_lint_options(doc, configs) {
            Some(options) => match lint_sql(&doc.text, &options) {
                Ok(findings) => diagnostics_for(&doc.text, &findings),
                Err(_) => Vec::new(), // mid-edit / unparseable: clear, don't spam
            },
            None => Vec::new(), // out of `paths` scope: clear any stale diagnostics
        }
    } else {
        Vec::new()
    };
    send_diagnostics(connection, &doc.uri, diagnostics)
}

/// Resolve the [`crate::LintOptions`] to lint `doc` with, or `None` if it's out of
/// the resolved config's `paths` scope (§10 of the LSP design doc) and must not be
/// linted at all — the shared gate `publish` and `on_code_action` both consult. A
/// document with no file path (untitled buffer) or a non-`file` URI always lints
/// with defaults: no config is discoverable for it, so `Config::in_scope`'s
/// empty-`paths` default (in scope) would apply anyway.
fn resolve_lint_options(doc: &Document, configs: &mut ConfigCache) -> Option<crate::LintOptions> {
    match uri_to_path(doc.uri.as_str()) {
        Some(path) if configs.should_lint(&path) => Some(configs.options_for(&path)),
        Some(_) => None,
        None => Some(crate::LintOptions::default()),
    }
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
    connection.sender.send(Message::Notification(Notification {
        method: "textDocument/publishDiagnostics".to_string(),
        params: serde_json::to_value(params)?,
    }))?;
    Ok(())
}

/// Whether `path`'s file name is a pgsafe config file (`pgsafe.toml` / `.pgsafe.toml`),
/// matching `config::discover`'s candidates (`config::CANDIDATES` — the single source
/// of truth, so this can't drift from what discovery actually looks for).
fn is_config_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| config::CANDIDATES.contains(&name))
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
        let o1 = cache.options_for(&sql);
        assert!(o1.disabled_rules.contains("add-index-non-concurrent"));

        // Rewrite the config to re-enable the rule.
        std::fs::write(&cfg, "[rules]\n").unwrap();
        // Stale until invalidated.
        let o2 = cache.options_for(&sql);
        assert!(o2.disabled_rules.contains("add-index-non-concurrent"));
        // After invalidation, re-read reflects the new config.
        cache.invalidate_dir(dir.path());
        let o3 = cache.options_for(&sql);
        assert!(!o3.disabled_rules.contains("add-index-non-concurrent"));
    }

    /// Regression test for the bug where `ConfigCache` cached a fully-resolved
    /// `LintOptions` by directory: whichever file was looked up FIRST in a directory
    /// determined `disabled_rules` for every sibling looked up afterward, silently
    /// mis-suppressing rules for files a per-file `[[ignore]]` glob didn't actually
    /// match (or should have matched). Proves each sibling file gets its own correct
    /// `disabled_rules` from one shared `ConfigCache` (and therefore one shared,
    /// cached `Config`), independent of lookup order.
    #[test]
    fn per_file_ignore_globs_are_not_conflated_across_sibling_lookups() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join(".pgsafe.toml")).unwrap();
        writeln!(
            f,
            "[[ignore]]\npath = \"*_seed.sql\"\nrules = [\"truncate\"]"
        )
        .unwrap();
        drop(f);

        let matching = dir.path().join("9_seed.sql"); // matches the ignore glob
        let other = dir.path().join("0001_add_index.sql"); // does not

        // Non-matching file looked up first, then the matching one: the first lookup
        // must not poison the second.
        let mut cache = ConfigCache::default();
        let o_other = cache.options_for(&other);
        let o_matching = cache.options_for(&matching);
        assert!(
            !o_other.disabled_rules.contains("truncate"),
            "non-matching sibling must not have `truncate` disabled, got {:?}",
            o_other.disabled_rules
        );
        assert!(
            o_matching.disabled_rules.contains("truncate"),
            "matching file must have `truncate` disabled, got {:?}",
            o_matching.disabled_rules
        );

        // Reversed order, fresh cache: the matching file looked up first must not
        // poison the non-matching sibling looked up second either.
        let mut cache = ConfigCache::default();
        let o_matching = cache.options_for(&matching);
        let o_other = cache.options_for(&other);
        assert!(
            o_matching.disabled_rules.contains("truncate"),
            "matching file must have `truncate` disabled (matching-first order), got {:?}",
            o_matching.disabled_rules
        );
        assert!(
            !o_other.disabled_rules.contains("truncate"),
            "non-matching sibling must not have `truncate` disabled (matching-first order), got {:?}",
            o_other.disabled_rules
        );
    }

    /// Regression test for the bug where the LSP hardcoded `assume_in_transaction` to
    /// `false` at every call site instead of reading the resolved config's
    /// `in_transaction` key, silently diverging from the CLI (which computes
    /// `args.in_transaction || config.in_transaction.unwrap_or(false)`) for the
    /// concurrently-in-transaction rule. Proves `options_for` now derives the flag from
    /// the cached `Config` — parity with the CLI for the same file + `.pgsafe.toml`.
    #[test]
    fn options_for_honors_config_in_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join(".pgsafe.toml")).unwrap();
        writeln!(f, "in_transaction = true").unwrap();
        drop(f);
        let sql = dir.path().join("a.sql");

        let mut cache = ConfigCache::default();
        let opts = cache.options_for(&sql);
        assert!(
            opts.assume_in_transaction,
            "expected assume_in_transaction=true from `.pgsafe.toml`'s `in_transaction = true`"
        );
    }

    /// Default/absent config (no `.pgsafe.toml`, or one that omits `in_transaction`)
    /// must still yield `assume_in_transaction = false` — the LSP shouldn't invent a
    /// transaction assumption the CLI wouldn't make either.
    #[test]
    fn options_for_defaults_to_no_transaction_when_config_absent() {
        let dir = tempfile::tempdir().unwrap();
        let sql = dir.path().join("a.sql");

        let mut cache = ConfigCache::default();
        let opts = cache.options_for(&sql);
        assert!(!opts.assume_in_transaction);
    }

    /// `should_lint` gates on the config's `paths` globs (§10 of the LSP design doc):
    /// a file under a matching directory is in scope, a sibling outside the globs is
    /// not — matched relative to the config's directory, same as `options_for`.
    #[test]
    fn should_lint_true_for_matching_path_false_for_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join(".pgsafe.toml")).unwrap();
        writeln!(f, "paths = [\"migrations/**\"]").unwrap();
        drop(f);

        let matching = dir.path().join("migrations").join("0001.sql");
        let sibling = dir.path().join("queries").join("q.sql");

        let mut cache = ConfigCache::default();
        assert!(cache.should_lint(&matching));
        assert!(!cache.should_lint(&sibling));
    }

    /// No `paths` key at all is the unset default: lint everything, same as today.
    #[test]
    fn should_lint_defaults_true_when_paths_unset() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join(".pgsafe.toml")).unwrap();
        writeln!(f, "[rules]\ndrop-table = false").unwrap();
        drop(f);

        let sql = dir.path().join("anything.sql");
        let mut cache = ConfigCache::default();
        assert!(cache.should_lint(&sql));
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
