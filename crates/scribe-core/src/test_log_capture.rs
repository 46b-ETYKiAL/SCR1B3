//! Minimal, dependency-light `tracing` capture harness for the silent-failure
//! logging tests.
//!
//! scribe-core has no shared capture helper, so this installs a custom
//! [`tracing_subscriber::Layer`] for the duration of a closure that records
//! every event's level + a flattened `message + fields` string into a shared
//! buffer. Tests then assert that a given path emitted the expected level +
//! substring, and — critically — that no path/secret token leaks at `warn`+.
//!
//! Field flattening matters for the leak check: a secret could ride in a FIELD
//! (e.g. `error_kind = ...`) rather than the message, so the visitor records
//! every field's value, not just `message`.

use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

/// A captured event: its level and a flattened `message [field=value …]` string.
type Record = (Level, String);

/// Shared, thread-safe handle to the captured records.
#[derive(Clone, Default)]
pub(crate) struct CapturedLogs(Arc<Mutex<Vec<Record>>>);

impl CapturedLogs {
    /// All captured records (level + flattened text), in emission order.
    pub(crate) fn records(&self) -> Vec<Record> {
        self.0.lock().expect("captured-logs mutex").clone()
    }

    /// `true` when ANY record at `level` contains `needle`.
    pub(crate) fn has(&self, level: Level, needle: &str) -> bool {
        self.records()
            .iter()
            .any(|(lvl, text)| *lvl == level && text.contains(needle))
    }

    /// The concatenated text of every record at `warn` or MORE severe (i.e.
    /// `warn` + `error`; in `tracing`, `ERROR < WARN`). Used to assert that a
    /// secret/path token never appears at user-visible severity.
    pub(crate) fn warn_plus_text(&self) -> String {
        self.records()
            .iter()
            .filter(|(lvl, _)| *lvl <= Level::WARN)
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Visitor that flattens an event's `message` + all fields into one string.
struct FlattenVisitor {
    buf: String,
}

impl FlattenVisitor {
    fn push(&mut self, piece: &str) {
        if !self.buf.is_empty() {
            self.buf.push(' ');
        }
        self.buf.push_str(piece);
    }
}

impl Visit for FlattenVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            let piece = value.to_string();
            self.push(&piece);
        } else {
            let piece = format!("{}={value}", field.name());
            self.push(&piece);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let piece = format!("{value:?}");
            self.push(&piece);
        } else {
            let piece = format!("{}={value:?}", field.name());
            self.push(&piece);
        }
    }
}

/// The capturing layer.
struct CaptureLayer {
    logs: CapturedLogs,
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = FlattenVisitor { buf: String::new() };
        event.record(&mut visitor);
        let level = *event.metadata().level();
        self.logs
            .0
            .lock()
            .expect("captured-logs mutex")
            .push((level, visitor.buf));
    }
}

/// Run `f` with a capturing subscriber installed for the current thread, handing
/// it the [`CapturedLogs`] handle to assert against.
pub(crate) fn with_captured_logs<R>(f: impl FnOnce(&CapturedLogs) -> R) -> R {
    // ROOT CAUSE of the intermittent `backup_corrupt_config_logs_error_*` flake: in
    // a test binary no GLOBAL default subscriber is ever installed (`main.rs`
    // installs one, but that never runs under `cargo test`). `tracing` caches each
    // callsite's `Interest` the FIRST time it is hit; with no subscriber present
    // that interest is cached as `never` — permanently, process-wide, across every
    // thread. A thread-local `with_default` does NOT rebuild that cache. So if any
    // non-capturing test hits the `backup_corrupt_config` `error!` callsite before a
    // capturing test does, the callsite is disabled forever and the capture silently
    // misses its line — order-dependent, parallel-only, passes-in-isolation. The
    // sibling `scribe-app/src/log_capture.rs` already carried this fix; the core
    // harness never got it.
    //
    // Fix: install a permanent, SILENT, TRACE-level global default once. It emits
    // nothing (a bare registry + LevelFilter, no fmt layer) but keeps every
    // callsite's interest ENABLED so the per-capture thread-local layer below always
    // receives events. The serial lock then keeps concurrent captures from
    // interleaving on the shared interest cache; poison is recovered so a panic
    // under capture does not cascade.
    static GLOBAL_INIT: std::sync::Once = std::sync::Once::new();
    GLOBAL_INIT.call_once(|| {
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(tracing_subscriber::filter::LevelFilter::TRACE),
        );
    });
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let logs = CapturedLogs::default();
    let layer = CaptureLayer { logs: logs.clone() };
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, || f(&logs))
}
