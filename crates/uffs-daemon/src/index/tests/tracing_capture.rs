// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Mini `tracing::Subscriber` scaffold for daemon-side observability
//! contract tests.
//!
//! Implements `tracing::Subscriber` directly (not via
//! `tracing-subscriber::Layer`) so the parallel-test interaction
//! with `tracing-core`'s global callsite-interest cache is
//! deterministic — see the doc comment on [`EventLog::register_callsite`]
//! for the full rationale and the race we hit on the previous
//! `tracing_subscriber::Layered<EventLog, Registry>` design.
//!
//! Used by:
//!
//! * [`super::idle_demote::shard_transition_events_emitted_on_demote_and_promote`]
//!   — Phase 3 task 3.9 — pins the `letter` / `from` / `to` /
//!   `reason` / `freed_mb` / `restored_mb` / `last_query_at_ms`
//!   field contract on the canonical `shard.transition` event for
//!   the demote-then-promote round-trip.
//! * `crate::cache::journal_loop::tests::compact_cache_save_log` — pins the
//!   literal `"compact-cache save"` message text the Phase 7 24-h soak harness
//!   greps for (visibility raised to `pub(crate)` in 2026-05-13 to share the
//!   scaffold across crate-internal modules).
//!
//! Helpers are intentionally minimal — only the fields and methods
//! the contract tests actually assert on are surfaced.  Sibling
//! tests that need a richer capture surface should extend this
//! module rather than re-implement.

use alloc::sync::Arc;
use std::sync::Mutex;

/// One captured tracing event.
#[derive(Debug, Clone)]
pub(crate) struct CapturedEvent {
    pub(crate) target: String,
    pub(crate) level: tracing::Level,
    /// `(field_name, stringified_value)` pairs.
    pub(crate) fields: Vec<(String, String)>,
}

impl CapturedEvent {
    /// String value of `field_name`, or `None` when the field was
    /// not present on this event.  Returns `&str` (not owned) so the
    /// test's `assert_eq!` reads naturally.
    pub(crate) fn field(&self, field_name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(name, _)| name == field_name)
            .map(|(_, value)| value.as_str())
    }

    /// `true` iff the event carries a field named `field_name`,
    /// regardless of its value.  Used for fields whose value is
    /// dynamic (e.g. `freed_mb` / `restored_mb`) and the test only
    /// pins the *presence*, not the magnitude.
    pub(crate) fn has_field(&self, field_name: &str) -> bool {
        self.fields.iter().any(|(name, _)| name == field_name)
    }
}

/// Thread-safe in-memory event log.  Cloned into the
/// `tracing::Subscriber` impl and the test asserts against the
/// shared `Arc<Mutex<...>>`.
#[derive(Default, Clone)]
pub(crate) struct EventLog(Arc<Mutex<Vec<CapturedEvent>>>);

impl EventLog {
    pub(crate) fn events(&self) -> Vec<CapturedEvent> {
        self.0.lock().unwrap().clone()
    }
}

/// Implements [`tracing::Subscriber`] *directly* (no
/// `tracing-subscriber::Layer` wrapping) so the parallel-test
/// interaction with `tracing`'s global callsite-interest cache is
/// deterministic:
///
/// * `register_callsite` returns `Interest::always` so the cache pins the
///   callsite as "always interested" once we've registered it.
/// * `enabled` returns `true` for every metadata so no filtering happens below
///   the macro level (the `Interest::always` already implies this).
/// * `max_level_hint` returns `LevelFilter::TRACE` so the static
///   `LevelFilter::current()` consulted at the macro level *before* dispatch
///   can never be lower than `TRACE` while this subscriber is the thread-local
///   default — preventing another subscriber's lower hint from silently
///   dropping `INFO`-level events.
///
/// The previous `tracing_subscriber::Layered<EventLog, Registry>`
/// implementation hit a race in parallel test runs: the inner
/// `Registry::register_callsite` returned `Interest::sometimes()`,
/// the outer `Layer::register_callsite` override didn't propagate
/// (`Layered` AND-combines them as `sometimes`), and sibling tests on
/// other threads racing through `tracing::info!` callsites pinned the
/// global per-callsite cache to `never` before we could rebuild it.
/// The direct `Subscriber` impl plus the dummy second `Dispatch` held
/// in the test body together pin the cache to `always` deterministically.
impl tracing::Subscriber for EventLog {
    fn register_callsite(
        &self,
        _metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }

    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::TRACE)
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::Id {
        // Span IDs are not inspected by the test; return a stable
        // non-zero placeholder so `tracing` is happy.
        tracing::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let metadata = event.metadata();
        let mut visitor = FieldCapture::default();
        event.record(&mut visitor);
        self.0.lock().unwrap().push(CapturedEvent {
            target: metadata.target().to_owned(),
            level: *metadata.level(),
            fields: visitor.fields,
        });
    }

    fn enter(&self, _span: &tracing::Id) {}

    fn exit(&self, _span: &tracing::Id) {}
}

/// `tracing::field::Visit` impl that converts every recorded field
/// into a `(name, stringified_value)` pair.
///
/// Internal to this module — sibling tests interact with
/// [`CapturedEvent::field`] / [`CapturedEvent::has_field`] which read
/// the post-visit `(name, value)` vector.
#[derive(Default)]
struct FieldCapture {
    fields: Vec<(String, String)>,
}

impl tracing::field::Visit for FieldCapture {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.fields
            .push((field.name().to_owned(), value.to_owned()));
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields
            .push((field.name().to_owned(), value.to_string()));
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields
            .push((field.name().to_owned(), value.to_string()));
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields
            .push((field.name().to_owned(), value.to_string()));
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn core::fmt::Debug) {
        // The `tracing::info!(letter = %x.to_ascii_uppercase(), ...)`
        // form goes through `record_debug` because `%` selects the
        // `Display` adapter and the underlying `Field` is recorded
        // via `Debug`.  We strip the surrounding quotes that
        // `Debug` adds for strings so the test asserts read
        // naturally.
        let raw = format!("{value:?}");
        let stripped = raw
            .strip_prefix('"')
            .and_then(|tail| tail.strip_suffix('"'))
            .map(str::to_owned)
            .unwrap_or(raw);
        self.fields.push((field.name().to_owned(), stripped));
    }
}
