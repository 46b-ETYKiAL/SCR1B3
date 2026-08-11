//! Language Server Protocol client.
//!
//! A minimal, pure-Rust LSP client: spawn a user-configured language server,
//! complete the `initialize` handshake, and stream `publishDiagnostics` back to
//! the UI over a channel. Engine-agnostic (no AI) — it speaks standard LSP to
//! whatever server the user installs. Missing/unconfigured servers degrade
//! gracefully (no crash).

pub mod protocol;
pub mod sync;

pub use protocol::Diagnostic;
pub use sync::{
    byte_offset_of_position, utf16_position, ChangeDebouncer, ContentChange, LineIndex,
    TextDocumentSyncKind,
};

use serde_json::{json, Value};
use std::io::{BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

/// `sync_kind` slot value meaning "the server has not told us yet".
///
/// A real kind is 0/1/2, so `u8::MAX` cannot collide with one. Until the
/// `initialize` result lands we fall back to FULL sync, which every server
/// accepts: the spec says a content change with no `range` IS the whole
/// document, so a full-document change is legal even for a server that will
/// later declare `Incremental`.
const SYNC_KIND_UNKNOWN: u8 = u8::MAX;

/// The document this client has open, and the state needed to send a
/// well-formed `didChange` for it.
#[derive(Debug, Clone)]
struct OpenDoc {
    uri: String,
    /// Monotonic per-document version. `didOpen` is 1; every `didChange`
    /// increments. A server rejects a change whose version did not advance.
    version: i64,
    /// The text the server currently believes the document holds. An
    /// incremental change's range is computed against THIS, not against
    /// whatever the editor showed last frame — otherwise a dropped/debounced
    /// intermediate state would silently desynchronise the two.
    text: String,
}

/// One language server: the command to run + the languages it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspServerConfig {
    pub command: String,
    pub args: Vec<String>,
    /// Language ids / file extensions this server handles (e.g. ["rs"]).
    pub languages: Vec<String>,
}

/// Registry of configured servers. Ships sensible defaults; the user can add
/// more via config. Servers are opt-in — absence means "no LSP for this lang".
#[derive(Debug, Clone, Default)]
pub struct LspRegistry {
    servers: Vec<LspServerConfig>,
}

impl LspRegistry {
    /// Common open-source servers (used only if the user has them installed).
    pub fn with_defaults() -> Self {
        let s = |command: &str, args: &[&str], langs: &[&str]| LspServerConfig {
            command: command.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            languages: langs.iter().map(|l| l.to_string()).collect(),
        };
        Self {
            servers: vec![
                s("rust-analyzer", &[], &["rs"]),
                s("pylsp", &[], &["py"]),
                s(
                    "typescript-language-server",
                    &["--stdio"],
                    &["ts", "tsx", "js", "jsx"],
                ),
                s("gopls", &[], &["go"]),
                s("clangd", &[], &["c", "cc", "cpp", "h", "hpp"]),
            ],
        }
    }

    pub fn add(&mut self, cfg: LspServerConfig) {
        self.servers.push(cfg);
    }

    /// The server (if any) configured for a language id / extension.
    pub fn for_language(&self, lang: &str) -> Option<&LspServerConfig> {
        self.servers
            .iter()
            .find(|s| s.languages.iter().any(|l| l == lang))
    }
}

/// `initialize` request params for a workspace root.
pub fn initialize_params(root_uri: &str) -> Value {
    json!({
        "processId": std::process::id(),
        "rootUri": root_uri,
        "capabilities": {
            "textDocument": {
                "publishDiagnostics": { "relatedInformation": true },
                "completion": { "completionItem": { "snippetSupport": false } },
                "hover": {},
                "definition": {}
            }
        },
        "clientInfo": { "name": "SCR1B3", "version": env!("CARGO_PKG_VERSION") }
    })
}

/// `textDocument/didOpen` params.
pub fn did_open_params(uri: &str, language_id: &str, text: &str) -> Value {
    json!({
        "textDocument": { "uri": uri, "languageId": language_id, "version": 1, "text": text }
    })
}

/// Drive a writer thread that owns `stdin` and serialises every outgoing
/// message in FIFO order. This is the root-cause fix for "a full stdin pipe
/// freezes the UI": the egui frame thread enqueues via a channel
/// ([`std::sync::mpsc::Sender::send`] never blocks on an unbounded channel) and
/// the *writer thread* — never the frame thread — is the one that blocks on a
/// stalled `write_all`/`flush`. A single writer draining a FIFO channel also
/// preserves the on-the-wire ordering callers expect (`initialize` before
/// `initialized` before `didOpen`, …).
///
/// The thread exits when the [`Sender`] is dropped: `recv()` returns `Err`, we
/// flush and return. It is the same off-thread idiom as the diagnostics reader
/// thread spawned alongside it.
fn run_writer_loop<W: Write>(mut stdin: W, rx: Receiver<Value>) {
    // FIFO drain: one message at a time, in the exact order they were enqueued.
    while let Ok(msg) = rx.recv() {
        // A broken pipe (server died / closed stdin) ends the loop — there is
        // nothing left to write to. Best-effort: the Drop path still reaps the
        // child regardless.
        if protocol::write_message(&mut stdin, &msg).is_err() {
            break;
        }
    }
    // Sender dropped (graceful shutdown) or the pipe broke: flush whatever the
    // OS still buffers, then let `stdin` drop (closing the write end, which the
    // server observes as EOF).
    let _ = stdin.flush();
}

/// Drive the reader side: decode framed messages from the server, learn the
/// declared document-sync kind from the `initialize` result, and forward every
/// non-empty diagnostic batch to the UI.
///
/// Split out of the spawn closure (the same shape as [`run_writer_loop`]) so
/// the whole decode → observe → forward path is exercised over an in-memory
/// stream — the sync-kind capture in particular, which a real language server
/// would otherwise be the only way to reach.
///
/// The sync kind is FIRST-WRITE-WINS: a server's `initialize` result is
/// definitive, and a later message that happens to look like one (a
/// `workspace/configuration` echo, a proxied second handshake) must not be able
/// to retarget an in-flight document's change encoding.
fn run_reader_loop<R: std::io::BufRead>(
    mut reader: R,
    tx: &Sender<Vec<Diagnostic>>,
    sync_kind: &AtomicU8,
) {
    loop {
        match protocol::read_message(&mut reader) {
            Ok(Some(msg)) => {
                if sync_kind.load(Ordering::Relaxed) == SYNC_KIND_UNKNOWN {
                    if let Some(k) = TextDocumentSyncKind::from_initialize_result(&msg) {
                        sync_kind.store(k.to_wire(), Ordering::Relaxed);
                    }
                }
                let diags = protocol::parse_publish_diagnostics(&msg);
                if !diags.is_empty() && tx.send(diags).is_err() {
                    // UI dropped the receiver — ordinary teardown, not a
                    // failure of the server. Debug, not warn.
                    tracing::debug!(
                        target: "scribe::lsp",
                        "language-server reader stopped: diagnostics receiver dropped"
                    );
                    break;
                }
            }
            // Clean EOF: the server closed stdout (exited / was reaped).
            // Diagnostics will no longer update — a recoverable degrade.
            Ok(None) => {
                tracing::warn!(
                    target: "scribe::lsp",
                    reason = "eof",
                    "language-server reader stopped: server closed the connection (diagnostics will no longer update)"
                );
                break;
            }
            // Malformed frame or broken pipe: the diagnostics stream dies
            // here. Log the error KIND only (never frame/buffer content).
            Err(e) => {
                tracing::warn!(
                    target: "scribe::lsp",
                    reason = "read-error",
                    error_kind = ?e.kind(),
                    "language-server reader stopped: unreadable frame or broken pipe (diagnostics will no longer update)"
                );
                break;
            }
        }
    }
}

/// A running LSP server connection. Diagnostics arrive on `diagnostics`.
///
/// Outgoing messages are never written on the caller's (egui frame) thread:
/// they are enqueued on `outgoing` and drained by a dedicated writer thread
/// that owns the child's `stdin`. A slow or stalled server can therefore never
/// block the UI — at worst the writer thread blocks, and the channel buffers.
pub struct LspClient {
    child: Child,
    /// Enqueue outgoing framed messages. `send` is non-blocking; the writer
    /// thread performs the actual (potentially blocking) `write_all`/`flush`.
    /// `Option` so [`Drop`] can take it and drop it to signal shutdown before
    /// joining the writer thread.
    outgoing: Option<Sender<Value>>,
    /// Handle to the writer thread, joined on [`Drop`] after the sender is
    /// dropped so `stdin` is flushed + closed before we reap the child.
    writer: Option<JoinHandle<()>>,
    next_id: AtomicI64,
    pub diagnostics: Receiver<Vec<Diagnostic>>,
    /// The sync kind the server declared in its `initialize` result, written by
    /// the reader thread (which is the only place that sees the result) and read
    /// by [`LspClient::flush_pending_change`] on the frame thread.
    /// [`SYNC_KIND_UNKNOWN`] until the handshake completes.
    sync_kind: Arc<AtomicU8>,
    /// The open document + the text the server last saw. `None` before
    /// `did_open`.
    open: Option<OpenDoc>,
    /// Coalesces a burst of keystrokes into one `didChange`.
    debouncer: ChangeDebouncer,
}

impl LspClient {
    /// Spawn the server, send `initialize` + `initialized`, and start a reader
    /// thread that forwards diagnostics. Returns an error if the command can't
    /// be launched (caller degrades gracefully).
    pub fn spawn(cfg: &LspServerConfig, root_uri: &str) -> std::io::Result<Self> {
        let mut child = Command::new(&cfg.command)
            .args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        let (tx, rx): (Sender<Vec<Diagnostic>>, Receiver<Vec<Diagnostic>>) =
            std::sync::mpsc::channel();
        // Shared with the reader thread, which is the only place the
        // `initialize` RESULT is ever seen. Written once (first result wins),
        // read on the frame thread when building a `didChange`.
        let sync_kind = Arc::new(AtomicU8::new(SYNC_KIND_UNKNOWN));
        let reader_sync_kind = Arc::clone(&sync_kind);
        std::thread::spawn(move || {
            run_reader_loop(BufReader::new(stdout), &tx, &reader_sync_kind);
        });

        // Writer thread: owns `stdin`, drains `out_rx` FIFO. Every outgoing
        // message — including the handshake below — flows through this thread,
        // so no `write_all`/`flush` ever runs on the caller's frame thread.
        let (out_tx, out_rx): (Sender<Value>, Receiver<Value>) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || run_writer_loop(stdin, out_rx));

        let next_id = AtomicI64::new(1);
        // Enqueue the handshake. `send` is non-blocking; ordering is guaranteed
        // by the single FIFO writer (initialize → initialized → any later
        // did_open). A send error here means the writer thread already died
        // (the spawn failed pathologically) — surface it as a broken pipe so
        // the caller degrades gracefully, exactly as a failed write would have.
        let init = protocol::request(
            next_id.fetch_add(1, Ordering::Relaxed),
            "initialize",
            initialize_params(root_uri),
        );
        let send = |m: Value| -> std::io::Result<()> {
            out_tx
                .send(m)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "lsp writer gone"))
        };
        send(init)?;
        send(protocol::notification("initialized", json!({})))?;

        Ok(Self {
            child,
            outgoing: Some(out_tx),
            writer: Some(writer),
            next_id,
            diagnostics: rx,
            sync_kind,
            open: None,
            debouncer: ChangeDebouncer::new(),
        })
    }

    /// Notify the server a document was opened.
    ///
    /// Non-blocking: the message is enqueued for the writer thread. Even if the
    /// server's stdin pipe is full, this returns promptly — the writer thread,
    /// not the caller, owns the blocking `write_all`. An `Err` means the writer
    /// thread has gone (server died); the caller degrades gracefully.
    ///
    /// Records the opened document so subsequent [`note_change`](Self::note_change)
    /// / [`flush_pending_change`](Self::flush_pending_change) calls can send a
    /// correctly-versioned `didChange` against it.
    pub fn did_open(&mut self, uri: &str, language_id: &str, text: &str) -> std::io::Result<()> {
        let msg = protocol::notification(
            "textDocument/didOpen",
            did_open_params(uri, language_id, text),
        );
        let result = self.enqueue(msg);
        // Record the document even if the enqueue failed: `open` describes what
        // this client is FOR, and a later flush is a no-op anyway once the
        // writer is gone (it returns the same broken-pipe error).
        self.open = Some(OpenDoc {
            uri: uri.to_string(),
            version: 1,
            text: text.to_string(),
        });
        self.debouncer = ChangeDebouncer::new();
        result
    }

    /// The URI of the document this client has open, if any. The editor uses it
    /// to make sure it only feeds changes for the buffer the server is actually
    /// tracking — a tab switch must not send the new tab's text against the old
    /// tab's URI.
    pub fn open_uri(&self) -> Option<&str> {
        self.open.as_ref().map(|o| o.uri.as_str())
    }

    /// The document-sync kind the server declared, or `None` while the
    /// `initialize` result has not arrived (or the server declared nothing).
    pub fn declared_sync_kind(&self) -> Option<TextDocumentSyncKind> {
        TextDocumentSyncKind::from_wire(i64::from(self.sync_kind.load(Ordering::Relaxed)))
    }

    /// The sync kind actually used to build a change: the declared one, or FULL
    /// while the handshake is still in flight. Full sync is the safe default —
    /// a rangeless content change is defined by the spec to mean "this is the
    /// whole document", which an incremental server also accepts.
    fn effective_sync_kind(&self) -> TextDocumentSyncKind {
        self.declared_sync_kind()
            .unwrap_or(TextDocumentSyncKind::Full)
    }

    /// Record the buffer's current text. Cheap and safe to call every frame:
    /// nothing is sent until the buffer has been quiet for
    /// [`sync::DEBOUNCE`], so a keystroke cannot spam the server.
    ///
    /// Text the server already holds, with nothing queued, is not a change and
    /// is ignored — otherwise merely OPENING a file would leave a permanently
    /// pending no-op edit.
    pub fn note_change(&mut self, text: &str, now: Instant) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        if !self.debouncer.is_pending() && open.text == text {
            return;
        }
        self.debouncer.note(text, now);
    }

    /// Send the debounced `didChange` if one is due.
    ///
    /// Returns `Ok(true)` when a change was actually enqueued. Call once per
    /// frame after [`note_change`](Self::note_change). Sends nothing when: no
    /// document is open, the quiet window has not elapsed, the text is
    /// unchanged since the server last saw it, or the server declared
    /// `TextDocumentSyncKind::NoSync`.
    pub fn flush_pending_change(&mut self, now: Instant) -> std::io::Result<bool> {
        let Some(text) = self.debouncer.take_due(now) else {
            return Ok(false);
        };
        let Some(open) = self.open.as_ref() else {
            return Ok(false);
        };
        let changes = sync::content_changes(&open.text, &text, self.effective_sync_kind());
        if changes.is_empty() {
            // Unchanged, or the server wants no change notifications. Still
            // adopt the text as the server's view so the next diff is computed
            // from a truthful base.
            if let Some(open) = self.open.as_mut() {
                open.text = text;
            }
            return Ok(false);
        }
        let (uri, version) = {
            let open = self.open.as_mut().expect("checked above");
            open.version += 1;
            (open.uri.clone(), open.version)
        };
        let msg = protocol::notification(
            "textDocument/didChange",
            protocol::did_change_params(&uri, version, &changes),
        );
        let result = self.enqueue(msg);
        // The change is on the wire (or the writer is gone and nothing more
        // will be); either way the server's view is now `text`.
        if let Some(open) = self.open.as_mut() {
            open.text = text;
        }
        result.map(|()| true)
    }

    /// The document version the server has been told about: `1` after
    /// `didOpen`, incremented by every `didChange` that actually went out.
    ///
    /// This is the honest "did the server hear about my edit?" signal — it
    /// advances only when a change was really built and enqueued, never merely
    /// because an edit was noted. `None` before `did_open`.
    pub fn document_version(&self) -> Option<i64> {
        self.open.as_ref().map(|o| o.version)
    }

    /// True while an edit has been noted but the quiet window has not elapsed,
    /// so nothing has gone to the server yet.
    pub fn has_pending_change(&self) -> bool {
        self.debouncer.is_pending()
    }

    /// Enqueue a framed message for the writer thread (FIFO, non-blocking).
    fn enqueue(&self, msg: Value) -> std::io::Result<()> {
        match &self.outgoing {
            Some(tx) => tx.send(msg).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "lsp writer gone")
            }),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "lsp client shutting down",
            )),
        }
    }

    fn id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Explicitly shut the server down now. Consuming `self` runs the same
    /// graceful-shutdown-then-reap path that [`Drop`] guarantees — so callers
    /// may use this for an eager shutdown, but a client that is simply dropped
    /// (language switch, app exit) is reaped just the same.
    pub fn shutdown(self) {
        // Drop does the work.
    }
}

/// A process handle the teardown can terminate. Abstracted so the [`Drop`]
/// ordering (C-1) is unit-testable against a fake without spawning a real,
/// genuinely-wedged server (which cannot be deterministically constructed).
trait Killable {
    /// Request termination NOW. On a real [`Child`] this breaks the stdin pipe,
    /// which is what UNBLOCKS an in-flight `write_all` in the writer thread so
    /// the subsequent join cannot hang.
    fn start_kill(&mut self);
    /// Reap the (now-terminated) process.
    fn reap(&mut self);
}

impl Killable for Child {
    fn start_kill(&mut self) {
        // `kill()` sends SIGKILL / TerminateProcess and closes our handles to
        // the child's pipes, breaking a full stdin pipe so a stalled
        // `write_all` returns with a broken-pipe error and the writer exits.
        let _ = self.kill();
    }
    fn reap(&mut self) {
        let _ = self.wait();
    }
}

/// C-1 root-cause fix: kill the child BEFORE joining the writer thread.
///
/// The prior order joined the writer FIRST. If that thread was inside a
/// blocking `write_all` to a live-but-not-reading server with a FULL stdin
/// pipe, dropping the sender could NOT interrupt the in-flight write, so the
/// join (and thus the egui frame thread, which runs `Drop`) blocked until the
/// pipe drained or broke. Killing the child first breaks the pipe, which
/// unblocks the write, which lets the join complete promptly. Reap order
/// (kill → join → wait) preserves the existing no-orphan-process semantics.
///
/// Generic over [`Killable`] + the join thunk so the ordering is asserted in a
/// unit test without a real wedged server.
fn teardown<K: Killable>(child: &mut K, join_writer: impl FnOnce()) {
    // 1. Kill FIRST — breaks the stdin pipe, unblocking any stalled writer.
    child.start_kill();
    // 2. Now the writer's `write_all` is guaranteed to return (broken pipe), so
    //    the join cannot hang.
    join_writer();
    // 3. Reap the terminated child (no orphaned process).
    child.reap();
}

impl Drop for LspClient {
    /// Reap the language server so we never leak an orphaned process. The
    /// default `Child` drop only *detaches* — a large server (rust-analyzer,
    /// clangd) would linger for the OS session. We enqueue the LSP graceful
    /// `shutdown`+`exit`, drop the sender so the writer thread flushes the
    /// queue + closes `stdin`, then **kill the child BEFORE joining the writer**
    /// (C-1) so a wedged server's full stdin pipe can never hang the join (and
    /// thus the egui frame thread), then `wait` to guarantee termination. All
    /// steps are best-effort; a child that has already exited makes `kill`/
    /// `wait` return harmless errors.
    fn drop(&mut self) {
        let id = self.id();
        // Enqueue the graceful shutdown handshake (FIFO — drained after any
        // already-queued message). Errors are ignored: if the writer is already
        // gone the child is reaped below regardless.
        let _ = self.enqueue(protocol::request(id, "shutdown", Value::Null));
        let _ = self.enqueue(protocol::notification("exit", Value::Null));
        // Drop the sender: the writer thread's `recv()` now returns `Err`, so it
        // flushes and exits, closing `stdin`. (A graceful server drains the
        // queue and exits on `exit`; a wedged one is force-broken by the kill
        // below.)
        drop(self.outgoing.take());
        let writer = self.writer.take();
        teardown(&mut self.child, move || {
            if let Some(handle) = writer {
                let _ = handle.join();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// C-1 regression: a real "wedged server with a full stdin pipe" cannot be
    /// deterministically constructed in a unit test, so we assert the load-
    /// bearing PROPERTY directly — `teardown` kills the child BEFORE joining the
    /// writer thread. A fake [`Killable`] records the kill event into a shared
    /// trace; the join thunk records its own event. The kill index MUST precede
    /// the join index, otherwise a stalled write could hang the join (and the
    /// egui frame thread) — the exact failure C-1 fixes.
    #[derive(Default)]
    struct FakeChild {
        trace: Rc<RefCell<Vec<&'static str>>>,
    }
    impl Killable for FakeChild {
        fn start_kill(&mut self) {
            self.trace.borrow_mut().push("kill");
        }
        fn reap(&mut self) {
            self.trace.borrow_mut().push("reap");
        }
    }

    #[test]
    fn teardown_kills_child_before_joining_writer() {
        let trace: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let mut child = FakeChild {
            trace: trace.clone(),
        };
        let t2 = trace.clone();
        teardown(&mut child, move || {
            // Stand-in for `writer.join()`. If this ran BEFORE the kill, a
            // full-pipe write could block it forever.
            t2.borrow_mut().push("join");
        });
        let order = trace.borrow();
        assert_eq!(
            order.as_slice(),
            &["kill", "join", "reap"],
            "child must be killed BEFORE the writer join so a stalled write can't hang it"
        );
        let kill_at = order.iter().position(|e| *e == "kill").unwrap();
        let join_at = order.iter().position(|e| *e == "join").unwrap();
        assert!(kill_at < join_at, "kill must strictly precede join");
    }

    #[test]
    fn registry_defaults_route_languages() {
        let r = LspRegistry::with_defaults();
        assert_eq!(r.for_language("rs").unwrap().command, "rust-analyzer");
        assert_eq!(r.for_language("py").unwrap().command, "pylsp");
        assert!(r
            .for_language("ts")
            .unwrap()
            .args
            .contains(&"--stdio".to_string()));
        assert!(r.for_language("nonsense").is_none()); // graceful absence
    }

    #[test]
    fn user_can_add_server() {
        let mut r = LspRegistry::default();
        assert!(r.for_language("zig").is_none());
        r.add(LspServerConfig {
            command: "zls".into(),
            args: vec![],
            languages: vec!["zig".into()],
        });
        assert_eq!(r.for_language("zig").unwrap().command, "zls");
    }

    #[test]
    fn initialize_params_shape() {
        let p = initialize_params("file:///proj");
        assert_eq!(p["rootUri"], "file:///proj");
        assert_eq!(p["clientInfo"]["name"], "SCR1B3");
        assert!(p["capabilities"]["textDocument"]["publishDiagnostics"].is_object());
    }

    #[test]
    fn did_open_params_shape() {
        let p = did_open_params("file:///x.rs", "rust", "fn main(){}");
        assert_eq!(p["textDocument"]["languageId"], "rust");
        assert_eq!(p["textDocument"]["version"], 1);
    }

    #[test]
    fn spawn_missing_server_errors_gracefully() {
        let cfg = LspServerConfig {
            command: "scr1b3-no-such-lsp-binary-xyz".into(),
            args: vec![],
            languages: vec!["rs".into()],
        };
        // No crash — just an Err the caller can ignore.
        assert!(LspClient::spawn(&cfg, "file:///x").is_err());
    }

    #[test]
    fn message_round_trips_through_content_length_framing() {
        // The Content-Length framing is the fragile part of the LSP transport;
        // exercise write_message -> read_message end-to-end in memory (the
        // real-process path was previously untested).
        let msg = protocol::request(7, "textDocument/hover", json!({ "x": 1 }));
        let mut buf: Vec<u8> = Vec::new();
        protocol::write_message(&mut buf, &msg).unwrap();
        assert!(
            buf.starts_with(b"Content-Length: "),
            "wire format must lead with a Content-Length header"
        );
        let mut reader = BufReader::new(&buf[..]);
        let back = protocol::read_message(&mut reader)
            .unwrap()
            .expect("one framed message");
        assert_eq!(back, msg);
    }

    // ---- writer-thread off-thread send: a full/stalled stdin pipe must never
    // block the caller (the egui frame thread). Proven against a mock `Write`
    // sink, with no child process. ----

    use std::sync::mpsc::{channel, Sender as MpscSender};
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::{Duration, Instant};

    /// A `Write` sink that blocks on the FIRST `write_all` until released, then
    /// records every subsequent write verbatim. Models a server whose stdin
    /// pipe is full: the writer thread stalls inside `write_all`, exactly where
    /// the old synchronous code stalled the frame thread.
    struct StallingSink {
        gate: Arc<Barrier>,
        gated: Mutex<bool>,
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for StallingSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            // Block exactly once, on the first write, until the test releases us.
            let mut already = self.gated.lock().unwrap();
            if !*already {
                *already = true;
                drop(already);
                self.gate.wait(); // stall here until the test signals
            }
            self.written.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn spawn_writer(sink: StallingSink) -> (MpscSender<Value>, std::thread::JoinHandle<()>) {
        let (tx, rx) = channel::<Value>();
        let handle = std::thread::spawn(move || run_writer_loop(sink, rx));
        (tx, handle)
    }

    #[test]
    fn send_returns_promptly_even_when_the_sink_is_stalled() {
        // The sink blocks on its first write. The caller enqueues several
        // messages; each `send` must return immediately (well under a generous
        // bound) — the blocking lives on the writer thread, never the caller.
        let gate = Arc::new(Barrier::new(2));
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = StallingSink {
            gate: gate.clone(),
            gated: Mutex::new(false),
            written: written.clone(),
        };
        let (tx, handle) = spawn_writer(sink);

        let start = Instant::now();
        for i in 1..=5 {
            tx.send(protocol::request(
                i,
                "textDocument/didOpen",
                json!({ "n": i }),
            ))
            .expect("enqueue never blocks");
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "caller sends must not block on a stalled sink (took {elapsed:?})"
        );

        // Now release the stalled writer; it drains the FIFO queue.
        gate.wait();
        drop(tx); // signal shutdown
        handle.join().unwrap();
        assert!(
            !written.lock().unwrap().is_empty(),
            "the writer thread did eventually write once unblocked"
        );
    }

    #[test]
    fn writer_preserves_message_order_and_framing_end_to_end() {
        // Enqueue a sequence; once the (briefly-stalled) writer drains it, the
        // bytes on the sink must decode back to the SAME messages in the SAME
        // order, with intact Content-Length framing.
        let gate = Arc::new(Barrier::new(2));
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = StallingSink {
            gate: gate.clone(),
            gated: Mutex::new(false),
            written: written.clone(),
        };
        let (tx, handle) = spawn_writer(sink);

        let msgs = vec![
            protocol::request(1, "initialize", json!({ "rootUri": "file:///p" })),
            protocol::notification("initialized", json!({})),
            protocol::notification("textDocument/didOpen", json!({ "v": 1 })),
            protocol::notification("textDocument/didChange", json!({ "v": 2 })),
        ];
        for m in &msgs {
            tx.send(m.clone()).expect("enqueue");
        }
        gate.wait(); // release the writer
        drop(tx);
        handle.join().unwrap();

        // Decode the recorded bytes back into messages and compare order-faithfully.
        let bytes = written.lock().unwrap().clone();
        let mut reader = BufReader::new(&bytes[..]);
        let mut decoded = Vec::new();
        while let Some(m) = protocol::read_message(&mut reader).unwrap() {
            decoded.push(m);
        }
        assert_eq!(decoded, msgs, "FIFO order + framing preserved on the wire");
    }

    #[test]
    fn dropping_the_sender_flushes_and_exits_the_writer_thread() {
        // Graceful shutdown: with no stall, dropping the sender drains the queue,
        // flushes, and the writer thread terminates (the Drop-path contract).
        let gate = Arc::new(Barrier::new(2));
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = StallingSink {
            gate: gate.clone(),
            gated: Mutex::new(true), // pre-released: never stalls
            written: written.clone(),
        };
        let (tx, rx) = channel::<Value>();
        let handle = std::thread::spawn(move || run_writer_loop(sink, rx));
        tx.send(protocol::notification("exit", Value::Null))
            .unwrap();
        drop(tx);
        // join must return — the writer saw the Recv error and exited.
        handle.join().unwrap();
        assert!(!written.lock().unwrap().is_empty());
    }

    /// Spawn a benign, long-lived, stdin-piped child so `LspClient`'s
    /// enqueue/id/Drop machinery can be exercised without a real language server.
    /// The child reads/holds stdin and stays alive until killed (which Drop does).
    /// Returns `None` if no such helper binary exists on this host (CI-skip — a
    /// genuine absence, never a false pass; the asserts below only run when a real
    /// client was constructed).
    fn spawn_benign_lsp_client() -> Option<LspClient> {
        let cfg = if cfg!(windows) {
            LspServerConfig {
                command: "cmd".into(),
                args: vec!["/c".into(), "pause".into()],
                languages: vec!["rs".into()],
            }
        } else {
            LspServerConfig {
                command: "cat".into(),
                args: vec![],
                languages: vec!["rs".into()],
            }
        };
        LspClient::spawn(&cfg, "file:///proj").ok()
    }

    #[test]
    fn id_returns_then_increments_the_next_id_counter() {
        // `id()` must return the CURRENT next_id and post-increment it. A mutation
        // pinning it to a constant (0 / 1 / -1) would hand out a fixed, colliding
        // request id and never advance — breaking response correlation. `spawn`
        // consumes id 1 for `initialize`, so the next allocations are 2, 3, ….
        let Some(client) = spawn_benign_lsp_client() else {
            return; // no benign child available on this host; nothing to assert
        };
        let first = client.id();
        let second = client.id();
        assert_eq!(first, 2, "first post-handshake id is 2 (initialize took 1)");
        assert_eq!(second, 3, "id must strictly increment on each call");
        assert!(second > first, "id must advance, never return a constant");
    }

    #[test]
    fn did_open_and_enqueue_succeed_then_fail_after_shutdown() {
        // Two assertions in one client lifecycle:
        //   1. With a live writer, `did_open` (and thus `enqueue`) returns Ok — the
        //      message is actually handed to the channel (kills `did_open -> Ok(())`
        //      ONLY together with #2, since a bare Ok(()) passes #1 too).
        //   2. After the outgoing sender is dropped, `enqueue`/`did_open` MUST
        //      return Err (writer gone). The `-> Ok(())` mutations on both
        //      `did_open` and `enqueue` would WRONGLY report success here — this is
        //      the discriminating case that kills them.
        let Some(mut client) = spawn_benign_lsp_client() else {
            return;
        };
        // Live path: enqueue succeeds.
        client
            .did_open("file:///x.rs", "rust", "fn main(){}")
            .expect("did_open enqueues while the writer is live");
        client
            .enqueue(protocol::notification("textDocument/didChange", json!({})))
            .expect("enqueue succeeds while the writer is live");

        // Drop the sender to simulate the writer being gone, then both calls fail.
        drop(client.outgoing.take());
        let did_open_err = client.did_open("file:///x.rs", "rust", "x").unwrap_err();
        assert_eq!(
            did_open_err.kind(),
            std::io::ErrorKind::BrokenPipe,
            "did_open must surface a broken pipe once the writer is gone, not Ok(())"
        );
        let enqueue_err = client
            .enqueue(protocol::notification("textDocument/didChange", json!({})))
            .unwrap_err();
        assert_eq!(
            enqueue_err.kind(),
            std::io::ErrorKind::BrokenPipe,
            "enqueue must surface a broken pipe once the writer is gone, not Ok(())"
        );
    }

    // ---- reader loop: diagnostics forwarding + sync-kind capture ----
    //
    // Driven over an in-memory framed stream through the REAL `run_reader_loop`
    // — the same idiom the writer tests use — so the handshake observation is
    // exercised without needing a real language server on the host.

    fn framed(msgs: &[Value]) -> Vec<u8> {
        let mut buf = Vec::new();
        for m in msgs {
            protocol::write_message(&mut buf, m).unwrap();
        }
        buf
    }

    /// Run the reader loop to EOF over `msgs`, returning the diagnostics it
    /// forwarded and the sync kind it learned.
    fn drive_reader(msgs: &[Value]) -> (Vec<Vec<Diagnostic>>, Option<TextDocumentSyncKind>) {
        let bytes = framed(msgs);
        let (tx, rx) = channel::<Vec<Diagnostic>>();
        let kind = AtomicU8::new(SYNC_KIND_UNKNOWN);
        run_reader_loop(BufReader::new(&bytes[..]), &tx, &kind);
        drop(tx);
        let batches: Vec<Vec<Diagnostic>> = rx.into_iter().collect();
        (
            batches,
            TextDocumentSyncKind::from_wire(i64::from(kind.load(Ordering::Relaxed))),
        )
    }

    fn diag_notification(line: u64, end_char: u64, message: &str) -> Value {
        protocol::notification(
            "textDocument/publishDiagnostics",
            json!({
                "uri": "file:///x.rs",
                "diagnostics": [{
                    "range": {"start": {"line": line, "character": 0},
                              "end":   {"line": line, "character": end_char}},
                    "severity": 1,
                    "message": message,
                }],
            }),
        )
    }

    #[test]
    fn the_reader_learns_the_sync_kind_from_the_handshake_and_still_forwards_diagnostics() {
        // Both jobs on one stream: without the sync-kind capture every server
        // would be driven as FULL sync; without the forwarding the editor shows
        // nothing.
        let (batches, kind) = drive_reader(&[
            json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": {
                "textDocumentSync": {"openClose": true, "change": 2}}}}),
            diag_notification(7, 4, "mismatched types"),
        ]);
        assert_eq!(
            kind,
            Some(TextDocumentSyncKind::Incremental),
            "the declared sync kind must be read off the initialize result"
        );
        assert_eq!(batches.len(), 1, "one diagnostic batch reached the UI");
        assert_eq!(batches[0][0].message, "mismatched types");
        assert_eq!((batches[0][0].line, batches[0][0].end_character), (7, 4));
    }

    #[test]
    fn a_server_that_declares_nothing_leaves_the_sync_kind_unknown() {
        let (_, kind) = drive_reader(&[
            json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": {}}}),
            diag_notification(0, 1, "x"),
        ]);
        assert_eq!(
            kind, None,
            "an unspecified sync kind must stay unknown so the caller's FULL \
             default applies — never be guessed at as Incremental"
        );
    }

    #[test]
    fn the_sync_kind_is_first_write_wins() {
        // A later message that looks like a handshake result must not retarget
        // the change encoding of a document already being edited.
        let (_, kind) = drive_reader(&[
            json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": {"textDocumentSync": 2}}}),
            json!({"jsonrpc": "2.0", "id": 9, "result": {"capabilities": {"textDocumentSync": 1}}}),
        ]);
        assert_eq!(
            kind,
            Some(TextDocumentSyncKind::Incremental),
            "the FIRST declared kind wins; a later one must not overwrite it"
        );
    }

    #[test]
    fn the_reader_stops_cleanly_on_a_malformed_frame_without_losing_earlier_diagnostics() {
        // Everything decoded BEFORE the bad frame must already be delivered.
        let mut bytes = framed(&[diag_notification(1, 2, "first")]);
        bytes.extend_from_slice(b"Content-Length: nonsense\r\n\r\n");
        let (tx, rx) = channel::<Vec<Diagnostic>>();
        let kind = AtomicU8::new(SYNC_KIND_UNKNOWN);
        run_reader_loop(BufReader::new(&bytes[..]), &tx, &kind);
        drop(tx);
        let batches: Vec<_> = rx.into_iter().collect();
        assert_eq!(batches.len(), 1, "the good frame was delivered");
        assert_eq!(batches[0][0].message, "first");
    }

    // ---- didChange: what actually reaches the server ----
    //
    // These re-point a LIVE client's outgoing side at an in-memory sink driven
    // by the REAL `run_writer_loop`, so the assertions are on the framed bytes
    // the server would receive — not on an internal flag.

    struct RecordingSink(Arc<Mutex<Vec<u8>>>);
    impl Write for RecordingSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Run `f` against a live client whose outgoing messages are recorded, and
    /// return the messages that reached the wire, decoded back from their
    /// `Content-Length` frames.
    fn wire_after(client: &mut LspClient, f: impl FnOnce(&mut LspClient)) -> Vec<Value> {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel::<Value>();
        let sink = RecordingSink(written.clone());
        let handle = std::thread::spawn(move || run_writer_loop(sink, rx));
        // Swap in the recording writer; retire the child-stdin one.
        let old_tx = client.outgoing.replace(tx);
        drop(old_tx);
        if let Some(old) = client.writer.replace(handle) {
            let _ = old.join();
        }

        f(client);

        // Close the recording writer and decode what it wrote.
        drop(client.outgoing.take());
        if let Some(h) = client.writer.take() {
            let _ = h.join();
        }
        let bytes = written.lock().unwrap().clone();
        let mut reader = BufReader::new(&bytes[..]);
        let mut out = Vec::new();
        while let Ok(Some(m)) = protocol::read_message(&mut reader) {
            out.push(m);
        }
        out
    }

    /// A live client, with the host-availability check made explicit: on Windows
    /// `cmd` always exists, so a `None` there is a real failure and not a skip.
    fn live_client() -> Option<LspClient> {
        let c = spawn_benign_lsp_client();
        assert!(
            c.is_some() || !cfg!(windows),
            "`cmd /c pause` must be spawnable on Windows — a None here is a \
             broken harness, not an absent dependency"
        );
        c
    }

    fn did_changes(msgs: &[Value]) -> Vec<&Value> {
        msgs.iter()
            .filter(|m| m["method"] == "textDocument/didChange")
            .collect()
    }

    #[test]
    fn a_typed_character_reaches_the_server_as_an_incremental_did_change() {
        // The whole feature: before this, EVERY `textDocument/didChange` string
        // in the tree was inside a test — diagnostics froze at the state the
        // file had when it was opened.
        let Some(mut client) = live_client() else {
            return;
        };
        client.sync_kind.store(
            TextDocumentSyncKind::Incremental.to_wire(),
            Ordering::Relaxed,
        );
        let t0 = Instant::now();
        let msgs = wire_after(&mut client, |c| {
            c.did_open("file:///x.rs", "rust", "fn mai() {}\n").unwrap();
            c.note_change("fn main() {}\n", t0);
            assert!(
                c.flush_pending_change(t0 + sync::DEBOUNCE).unwrap(),
                "the change was due and must have been sent"
            );
        });
        let changes = did_changes(&msgs);
        assert_eq!(changes.len(), 1, "exactly one didChange on the wire");
        let c = changes[0];
        assert_eq!(c["params"]["textDocument"]["uri"], "file:///x.rs");
        assert_eq!(
            c["params"]["textDocument"]["version"], 2,
            "didOpen is version 1, so the first change is version 2"
        );
        assert_eq!(
            c["params"]["contentChanges"][0]["text"], "n",
            "an incremental server receives the typed character, not the file"
        );
        assert_eq!(
            c["params"]["contentChanges"][0]["range"]["start"]["character"],
            6
        );
    }

    #[test]
    fn a_full_sync_server_receives_the_whole_document_and_no_range() {
        let Some(mut client) = live_client() else {
            return;
        };
        client
            .sync_kind
            .store(TextDocumentSyncKind::Full.to_wire(), Ordering::Relaxed);
        let t0 = Instant::now();
        let msgs = wire_after(&mut client, |c| {
            c.did_open("file:///x.rs", "rust", "a\n").unwrap();
            c.note_change("ab\n", t0);
            c.flush_pending_change(t0 + sync::DEBOUNCE).unwrap();
        });
        let changes = did_changes(&msgs);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0]["params"]["contentChanges"][0]["text"], "ab\n");
        assert!(
            changes[0]["params"]["contentChanges"][0]
                .get("range")
                .is_none(),
            "a full-sync change must carry no range"
        );
    }

    #[test]
    fn a_server_that_wants_no_change_notifications_gets_none() {
        let Some(mut client) = live_client() else {
            return;
        };
        client
            .sync_kind
            .store(TextDocumentSyncKind::NoSync.to_wire(), Ordering::Relaxed);
        let t0 = Instant::now();
        let msgs = wire_after(&mut client, |c| {
            c.did_open("file:///x.rs", "rust", "a\n").unwrap();
            c.note_change("ab\n", t0);
            assert!(
                !c.flush_pending_change(t0 + sync::DEBOUNCE).unwrap(),
                "nothing is sent to a NoSync server"
            );
        });
        assert!(
            did_changes(&msgs).is_empty(),
            "a NoSync server must receive no didChange at all, got: {msgs:?}"
        );
    }

    #[test]
    fn an_unknown_sync_kind_falls_back_to_full_rather_than_sending_nothing() {
        // Before the handshake result lands the editor must still sync — a
        // silent "wait for capabilities" would freeze diagnostics exactly as
        // the missing didChange did.
        let Some(mut client) = live_client() else {
            return;
        };
        assert_eq!(
            client.declared_sync_kind(),
            None,
            "precondition: `cmd /c pause` never sends an initialize result"
        );
        let t0 = Instant::now();
        let msgs = wire_after(&mut client, |c| {
            c.did_open("file:///x.rs", "rust", "a\n").unwrap();
            c.note_change("ab\n", t0);
            c.flush_pending_change(t0 + sync::DEBOUNCE).unwrap();
        });
        let changes = did_changes(&msgs);
        assert_eq!(changes.len(), 1, "the change is still sent");
        assert_eq!(
            changes[0]["params"]["contentChanges"][0]["text"], "ab\n",
            "the fallback is FULL sync (whole document, no range)"
        );
    }

    #[test]
    fn a_burst_of_keystrokes_reaches_the_server_as_one_message() {
        // Debounce, observed at the wire: five keystrokes, one didChange,
        // carrying the FINAL text.
        let Some(mut client) = live_client() else {
            return;
        };
        client
            .sync_kind
            .store(TextDocumentSyncKind::Full.to_wire(), Ordering::Relaxed);
        let t0 = Instant::now();
        let msgs = wire_after(&mut client, |c| {
            c.did_open("file:///x.rs", "rust", "").unwrap();
            for (i, s) in ["h", "he", "hel", "hell", "hello"].iter().enumerate() {
                let now = t0 + std::time::Duration::from_millis(10 * i as u64);
                c.note_change(s, now);
                // Every frame flushes; none is due inside the burst.
                assert!(
                    !c.flush_pending_change(now).unwrap(),
                    "no send while the user is still typing"
                );
            }
            assert!(c
                .flush_pending_change(t0 + std::time::Duration::from_millis(40) + sync::DEBOUNCE)
                .unwrap());
        });
        let changes = did_changes(&msgs);
        assert_eq!(
            changes.len(),
            1,
            "five keystrokes must produce ONE message, got {} — {msgs:?}",
            changes.len()
        );
        assert_eq!(
            changes[0]["params"]["contentChanges"][0]["text"], "hello",
            "and it carries the final text, not an intermediate keystroke"
        );
    }

    #[test]
    fn successive_changes_advance_the_version_and_diff_from_the_servers_view() {
        // Version must strictly increase (a server drops a non-advancing
        // change), and the SECOND diff must be computed against what the server
        // actually holds — not against the editor's previous frame.
        let Some(mut client) = live_client() else {
            return;
        };
        client.sync_kind.store(
            TextDocumentSyncKind::Incremental.to_wire(),
            Ordering::Relaxed,
        );
        let t0 = Instant::now();
        let msgs = wire_after(&mut client, |c| {
            c.did_open("file:///x.rs", "rust", "ab\n").unwrap();
            c.note_change("axb\n", t0);
            c.flush_pending_change(t0 + sync::DEBOUNCE).unwrap();
            let t1 = t0 + sync::DEBOUNCE * 2;
            c.note_change("axyb\n", t1);
            c.flush_pending_change(t1 + sync::DEBOUNCE).unwrap();
        });
        let changes = did_changes(&msgs);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0]["params"]["textDocument"]["version"], 2);
        assert_eq!(changes[1]["params"]["textDocument"]["version"], 3);
        assert_eq!(changes[0]["params"]["contentChanges"][0]["text"], "x");
        assert_eq!(
            changes[1]["params"]["contentChanges"][0]["text"], "y",
            "the second diff is against 'axb' (what the server holds), so only \
             'y' travels"
        );
        assert_eq!(
            changes[1]["params"]["contentChanges"][0]["range"]["start"]["character"],
            2
        );
        assert_eq!(client.document_version(), Some(3));
    }

    #[test]
    fn a_debounced_edit_that_lands_back_on_the_original_text_sends_nothing() {
        // Type a character and delete it inside the window: the server's view
        // never changed, so it must not be told it did.
        let Some(mut client) = live_client() else {
            return;
        };
        client
            .sync_kind
            .store(TextDocumentSyncKind::Full.to_wire(), Ordering::Relaxed);
        let t0 = Instant::now();
        let msgs = wire_after(&mut client, |c| {
            c.did_open("file:///x.rs", "rust", "ab\n").unwrap();
            c.note_change("abc\n", t0);
            c.note_change("ab\n", t0 + std::time::Duration::from_millis(20));
            assert!(!c
                .flush_pending_change(t0 + std::time::Duration::from_millis(20) + sync::DEBOUNCE)
                .unwrap());
        });
        assert!(did_changes(&msgs).is_empty(), "got: {msgs:?}");
        assert_eq!(
            client.document_version(),
            Some(1),
            "the version must NOT advance for a no-op edit"
        );
    }

    #[test]
    fn a_frame_loop_re_noting_the_unchanged_buffer_still_gets_its_edit_sent() {
        // The client's caller is a ~60fps frame loop that hands over the active
        // buffer EVERY frame, changed or not. If each of those restarted the
        // quiet window, the window would never elapse and no didChange would
        // EVER reach the server — the feature dead in exactly the way it looks
        // alive. Simulated here at the client level: one real edit, then many
        // idle frames re-noting the same text.
        let Some(mut client) = live_client() else {
            return;
        };
        client
            .sync_kind
            .store(TextDocumentSyncKind::Full.to_wire(), Ordering::Relaxed);
        let t0 = Instant::now();
        let msgs = wire_after(&mut client, |c| {
            c.did_open("file:///x.rs", "rust", "a\n").unwrap();
            c.note_change("ab\n", t0);
            for i in 0..60u64 {
                let now = t0 + std::time::Duration::from_millis(i * 16);
                c.note_change("ab\n", now); // unchanged: an idle frame
                let _ = c.flush_pending_change(now).unwrap();
            }
        });
        let changes = did_changes(&msgs);
        assert_eq!(
            changes.len(),
            1,
            "the edit must reach the server exactly once despite 60 idle \
             re-notes, got {} — {msgs:?}",
            changes.len()
        );
        assert_eq!(changes[0]["params"]["contentChanges"][0]["text"], "ab\n");
    }

    #[test]
    fn opening_a_file_and_idling_queues_nothing() {
        // A frame loop notes the buffer every frame from the moment the file
        // opens. Text the server already has is not a change.
        let Some(mut client) = live_client() else {
            return;
        };
        let t0 = Instant::now();
        let msgs = wire_after(&mut client, |c| {
            c.did_open("file:///x.rs", "rust", "unchanged\n").unwrap();
            for i in 0..30u64 {
                let now = t0 + std::time::Duration::from_millis(i * 16);
                c.note_change("unchanged\n", now);
                let _ = c.flush_pending_change(now).unwrap();
            }
            assert!(
                !c.has_pending_change(),
                "opening a file must not leave a permanently pending no-op edit"
            );
        });
        assert!(did_changes(&msgs).is_empty(), "got: {msgs:?}");
    }

    #[test]
    fn a_change_noted_before_any_did_open_is_inert() {
        let Some(mut client) = live_client() else {
            return;
        };
        let t0 = Instant::now();
        let msgs = wire_after(&mut client, |c| {
            c.note_change("typing into nothing", t0);
            assert!(!c.flush_pending_change(t0 + sync::DEBOUNCE).unwrap());
        });
        assert!(did_changes(&msgs).is_empty(), "got: {msgs:?}");
    }

    #[test]
    fn reopening_a_document_resets_the_debouncer_so_no_stale_text_is_sent() {
        // Switching files must not let the PREVIOUS file's pending text be
        // flushed against the NEW document's uri.
        let Some(mut client) = live_client() else {
            return;
        };
        client
            .sync_kind
            .store(TextDocumentSyncKind::Full.to_wire(), Ordering::Relaxed);
        let t0 = Instant::now();
        let msgs = wire_after(&mut client, |c| {
            c.did_open("file:///a.rs", "rust", "aaa\n").unwrap();
            c.note_change("aaa edited\n", t0);
            // No flush — the user switched files first.
            c.did_open("file:///b.rs", "rust", "bbb\n").unwrap();
            assert!(!c.flush_pending_change(t0 + sync::DEBOUNCE * 4).unwrap());
        });
        assert!(
            did_changes(&msgs).is_empty(),
            "the first file's pending text must not be sent against the \
             second file's uri, got: {msgs:?}"
        );
    }

    #[test]
    fn read_message_decodes_a_stream_of_two_then_eof() {
        let a = protocol::notification("initialized", json!({}));
        let b = protocol::request(2, "shutdown", Value::Null);
        let mut buf: Vec<u8> = Vec::new();
        protocol::write_message(&mut buf, &a).unwrap();
        protocol::write_message(&mut buf, &b).unwrap();
        let mut reader = BufReader::new(&buf[..]);
        assert_eq!(protocol::read_message(&mut reader).unwrap().unwrap(), a);
        assert_eq!(protocol::read_message(&mut reader).unwrap().unwrap(), b);
        // Clean EOF -> Ok(None), never an error.
        assert!(protocol::read_message(&mut reader).unwrap().is_none());
    }

    /// `open_uri` is the guard the editor uses to make sure a change is only
    /// ever sent for the buffer the server is actually tracking
    /// (`frame_tick.rs` compares it against the active tab's path). Nothing
    /// asserted its VALUE, so all three of its mutants survived: `-> None`
    /// (the editor concludes no document is open and syncs nothing, freezing
    /// diagnostics), `-> Some("")` and `-> Some("xyzzy")` (the comparison never
    /// matches any real tab, same freeze — or, worse, matches the WRONG tab if
    /// the constant ever collided).
    ///
    /// Three assertions, each discriminating: `None` before `did_open` kills
    /// both constant-`Some` mutants; the exact URI after `did_open` kills
    /// `-> None`; and re-opening a second document proves the value TRACKS the
    /// document rather than being any fixed string.
    #[test]
    fn open_uri_names_the_document_the_server_is_actually_tracking() {
        // `.expect`, not `else { return }`: `cat` (unix) / `cmd` (windows) is
        // always present, so a None is a broken harness — and a test that
        // silently returns is a mutant's best friend.
        let mut client = live_client().expect(
            "`cat` / `cmd /c pause` must be spawnable — a None here is a broken \
             harness, not an absent dependency",
        );

        assert_eq!(
            client.open_uri(),
            None,
            "before did_open the client tracks NO document — reporting some \
             constant uri here would make the editor sync an unopened buffer"
        );

        client
            .did_open("file:///proj/main.rs", "rust", "fn main() {}\n")
            .expect("did_open enqueues while the writer is live");
        assert_eq!(
            client.open_uri(),
            Some("file:///proj/main.rs"),
            "open_uri must report the uri that was actually opened"
        );

        client
            .did_open("file:///proj/other.rs", "rust", "fn other() {}\n")
            .expect("did_open enqueues while the writer is live");
        assert_eq!(
            client.open_uri(),
            Some("file:///proj/other.rs"),
            "re-opening must RETARGET: a value that stays on the first uri (or \
             on any constant) is what lets a tab switch send one file's text \
             against another file's uri"
        );
    }

    /// `has_pending_change` is the "an edit is noted but the quiet window has
    /// not elapsed" signal. `-> false` survived: a client that always claims
    /// nothing is queued makes a caller believe the edit already went out, so
    /// the server keeps serving diagnostics for stale text with nothing to
    /// indicate it. The complementary `-> true` mutant was already caught, so
    /// this pins the direction that was not.
    ///
    /// The clock is passed in, never slept on — the debouncer takes `now`
    /// explicitly, so the window is asserted with two instants and the test
    /// stays deterministic.
    #[test]
    fn has_pending_change_is_true_exactly_while_an_edit_is_waiting_out_the_window() {
        let mut client = live_client().expect(
            "`cat` / `cmd /c pause` must be spawnable — a None here is a broken \
             harness, not an absent dependency",
        );
        let t0 = Instant::now();

        assert!(
            !client.has_pending_change(),
            "a client with no document open has nothing queued"
        );
        client
            .did_open("file:///x.rs", "rust", "fn mai() {}\n")
            .expect("did_open enqueues while the writer is live");
        assert!(
            !client.has_pending_change(),
            "OPENING a file is not an edit — didOpen already carried the text"
        );

        client.note_change("fn main() {}\n", t0);
        assert!(
            client.has_pending_change(),
            "an edit inside the quiet window IS pending: reporting false here \
             tells the caller the server already has this text when it has not \
             been sent at all"
        );
        // Still pending part-way through the window: the flag tracks the queue,
        // not merely the instant of the keystroke.
        assert!(
            !client
                .flush_pending_change(t0 + sync::DEBOUNCE / 2)
                .expect("the writer is live"),
            "a flush before the quiet window elapses must send nothing"
        );
        assert!(
            client.has_pending_change(),
            "and it must leave the edit QUEUED — the flag tracks the queue, not \
             merely the instant of the keystroke"
        );

        assert!(
            client
                .flush_pending_change(t0 + sync::DEBOUNCE + Duration::from_millis(1))
                .expect("the writer is live"),
            "once the window has elapsed the change must actually be sent"
        );
        assert!(
            !client.has_pending_change(),
            "and the queue is empty again afterwards"
        );
    }
}
