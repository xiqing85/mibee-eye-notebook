//! Conversation-scoped model call-chain tracing (SPEC v1 §3.3).
//!
//! One conversation = one HTTP chat request (`origin:"chat"`) or one
//! voice wake-session incl. its follow-up turns (`origin:"voice"`).
//! Every model invoked on that conversation's answer path (decision
//! triage, VLM, cloud LLM, local LLM, TTS…) opens a span carrying the
//! call order (time offset), nesting, and the resources it consumed
//! (wall time, process-CPU delta, token counts where billed).
//!
//! The hub is a process-global bounded ring (precedent: [`crate::observe`])
//! so both the web routes (HTTP chat) and the root voice bridge can
//! record without threading state through every constructor. Traces are
//! best-effort telemetry: every fallible path below degrades to a no-op —
//! tracing must never break a conversation.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use serde::Serialize;
use tracing::Instrument;

/// Ring capacity — oldest conversations are dropped first (SPEC §3.3).
const MAX_CONVERSATIONS: usize = 200;
/// Per-conversation span cap; later spans are counted but not stored.
const MAX_SPANS_PER_CONV: usize = 64;

/// One model invocation inside a conversation (SPEC §3.3 span object).
#[derive(Debug, Clone, Serialize)]
pub struct ConvSpan {
    pub span_id: u64,
    pub parent_id: Option<u64>,
    pub model: String,
    pub variant: String,
    pub label: String,
    /// Milliseconds after the conversation started.
    pub start_ms: u64,
    pub duration_ms: u64,
    /// Process-wide CPU time delta during the call (best-effort
    /// attribution across the inference thread pool); `None` off Linux.
    pub cpu_ms: Option<u64>,
    /// "ok" | "error"
    pub status: String,
    pub tokens_prompt: Option<u64>,
    pub tokens_completion: Option<u64>,
    /// Free-form key/values (decision choice, grounded path, tts lang…).
    pub attributes: BTreeMap<String, String>,
}

/// List-item projection of one conversation (SPEC §3.3).
#[derive(Debug, Clone, Serialize)]
pub struct ConvSummary {
    pub id: String,
    pub origin: String,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub turns: u32,
    pub models: Vec<String>,
    /// "ok" | "partial" | "error"
    pub status: String,
    pub open: bool,
}

/// Full trace payload (SPEC §3.3 detail response).
#[derive(Debug, Clone, Serialize)]
pub struct ConvDetail {
    pub id: String,
    pub origin: String,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub turns: u32,
    pub open: bool,
    /// Sorted by `start_ms` (call order).
    pub spans: Vec<ConvSpan>,
}

struct ConvEntry {
    id: String,
    origin: String,
    started_at_ms: u64,
    started: Instant,
    spans: Vec<ConvSpan>,
    /// Span ids already handed out (finished spans keep their slot).
    next_span_id: u64,
    /// Spans currently executing (drives `open`).
    inflight: usize,
    turns: u32,
    any_ok: bool,
    any_err: bool,
    closed: bool,
    /// Finalised duration (set by `close`), else derived live.
    closed_duration: Option<Duration>,
    /// OTel root span — the same chain exports to OTLP when configured
    /// (SPEC appendix A #39-③): children created by [`ConvSpanGuard`]
    /// parent to this span via explicit `parent:` linkage (spawn_blocking
    /// breaks ambient context, so the parent is passed by value).
    otel: tracing::Span,
}

/// Process-global conversation trace ring.
pub struct ConvTraceHub {
    convs: std::sync::Mutex<VecDeque<Arc<std::sync::Mutex<ConvEntry>>>>,
    counter: std::sync::atomic::AtomicU64,
}

/// Global accessor (single instance for the process lifetime).
pub fn convtrace() -> &'static ConvTraceHub {
    static HUB: OnceLock<ConvTraceHub> = OnceLock::new();
    HUB.get_or_init(ConvTraceHub::new)
}

impl ConvTraceHub {
    fn new() -> Self {
        Self {
            convs: std::sync::Mutex::new(VecDeque::with_capacity(MAX_CONVERSATIONS)),
            counter: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Begin a new conversation and return its handle.
    pub fn start(&self, origin: &str) -> ConvHandle {
        let seq = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("c{:x}{:04x}", unix_now_ms(), seq & 0xffff);
        let otel = tracing::info_span!(
            "conversation",
            conversation.id = %id,
            origin,
            otel.name = format!("conversation/{origin}"),
        );
        let entry = Arc::new(std::sync::Mutex::new(ConvEntry {
            id: id.clone(),
            origin: origin.to_string(),
            started_at_ms: unix_now_ms(),
            started: Instant::now(),
            spans: Vec::new(),
            next_span_id: 1,
            inflight: 0,
            turns: 0,
            any_ok: false,
            any_err: false,
            closed: false,
            closed_duration: None,
            otel,
        }));
        let mut convs = self.convs.lock().expect("convtrace ring lock");
        if convs.len() >= MAX_CONVERSATIONS {
            convs.pop_front();
        }
        convs.push_back(Arc::clone(&entry));
        ConvHandle { entry }
    }

    /// Recent conversations, newest first (SPEC §3.3 list endpoint).
    pub fn list(&self, limit: usize) -> Vec<ConvSummary> {
        let convs = self.convs.lock().expect("convtrace ring lock");
        convs
            .iter()
            .rev()
            .take(limit)
            .filter_map(|e| summarize(&e.lock().expect("convtrace entry lock")))
            .collect()
    }

    /// One conversation by id (SPEC §3.3 detail endpoint).
    pub fn get(&self, id: &str) -> Option<ConvDetail> {
        let convs = self.convs.lock().expect("convtrace ring lock");
        convs
            .iter()
            .find(|e| e.lock().expect("convtrace entry lock").id == id)
            .map(|e| detail(&e.lock().expect("convtrace entry lock")))
    }

    /// Drop everything (test isolation).
    #[cfg(test)]
    pub fn clear(&self) {
        self.convs.lock().expect("convtrace ring lock").clear();
    }
}

fn summarize(e: &ConvEntry) -> Option<ConvSummary> {
    Some(ConvSummary {
        id: e.id.clone(),
        origin: e.origin.clone(),
        started_at_ms: e.started_at_ms,
        duration_ms: duration_of(e).as_millis() as u64,
        turns: e.turns,
        models: distinct_models(e),
        status: status_of(e),
        open: is_open(e),
    })
}

fn detail(e: &ConvEntry) -> ConvDetail {
    let mut spans = e.spans.clone();
    spans.sort_by_key(|s| s.start_ms);
    ConvDetail {
        id: e.id.clone(),
        origin: e.origin.clone(),
        started_at_ms: e.started_at_ms,
        duration_ms: duration_of(e).as_millis() as u64,
        turns: e.turns,
        open: is_open(e),
        spans,
    }
}

fn duration_of(e: &ConvEntry) -> Duration {
    if let Some(d) = e.closed_duration {
        return d;
    }
    let elapsed = e.started.elapsed();
    // Defensive cap for conversations whose owner never closed them
    // (crashed task, forgotten path): past five minutes of silence the
    // duration freezes at the last span end instead of growing forever.
    if elapsed.as_secs() > 300 {
        e.spans
            .iter()
            .map(|s| Duration::from_millis(s.start_ms + s.duration_ms))
            .max()
            .unwrap_or(Duration::ZERO)
    } else {
        elapsed
    }
}

fn is_open(e: &ConvEntry) -> bool {
    !e.closed && (e.inflight > 0 || e.started.elapsed().as_secs() < 5)
}

fn status_of(e: &ConvEntry) -> String {
    let s = match (e.any_ok, e.any_err) {
        (true, true) => "partial",
        (true, false) => "ok",
        (false, true) => "error",
        (false, false) => "ok", // no model called yet
    };
    s.to_string()
}

fn distinct_models(e: &ConvEntry) -> Vec<String> {
    let mut models: Vec<String> = e.spans.iter().map(|s| s.model.clone()).collect();
    models.sort();
    models.dedup();
    models
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Owner handle of one conversation. Clone it into spawned tasks; the
/// recorded trace lives in the global hub, not in the handle.
#[derive(Clone)]
pub struct ConvHandle {
    entry: Arc<std::sync::Mutex<ConvEntry>>,
}

impl ConvHandle {
    /// The conversation's conversation_id (stable across clones).
    pub fn id(&self) -> String {
        self.entry.lock().expect("convtrace entry lock").id.clone()
    }

    /// Count one dialogue turn (one user question on this conversation).
    pub fn add_turn(&self) {
        self.entry.lock().expect("convtrace entry lock").turns += 1;
    }

    /// Finalise the conversation (HTTP handlers call this before
    /// returning; the voice bridge when its 120 s slot expires).
    pub fn close(&self) {
        let mut e = self.entry.lock().expect("convtrace entry lock");
        if !e.closed {
            e.closed = true;
            e.closed_duration = Some(e.started.elapsed());
            // End the OTel root span now (drop the stored clone); guards
            // that still parent to it keep their own clones alive until
            // they finish.
            e.otel = tracing::Span::none();
        }
    }

    /// Open one model span on this conversation.
    pub fn span(
        &self,
        model: &str,
        variant: &str,
        label: &str,
        attributes: Vec<(String, String)>,
    ) -> ConvSpanGuard {
        let (span_id, otel_parent) = {
            let mut e = self.entry.lock().expect("convtrace entry lock");
            let id = e.next_span_id;
            e.next_span_id += 1;
            e.inflight += 1;
            (id, e.otel.clone())
        };
        ConvSpanGuard {
            entry: Arc::clone(&self.entry),
            span_id,
            parent: None,
            model: model.to_string(),
            variant: variant.to_string(),
            label: label.to_string(),
            started: Instant::now(),
            start_offset: self
                .entry
                .lock()
                .expect("convtrace entry lock")
                .started
                .elapsed(),
            cpu_start: crate::observe::process_cpu_time(),
            tokens_prompt: None,
            tokens_completion: None,
            attributes: attributes.into_iter().collect(),
            otel: tracing::info_span!(
                parent: otel_parent,
                "model_call",
                model,
                variant,
                conversation_model_label = label,
                otel.name = format!("model_call/{model}"),
            ),
            finished: false,
        }
    }
}

/// One in-flight model call on a conversation. Finish explicitly with
/// [`ConvSpanGuard::finish_ok`] / [`finish_err`]; dropping un-finished
/// records a successful span without token counts (mirrors the
/// observability `ModelCallGuard` contract).
pub struct ConvSpanGuard {
    entry: Arc<std::sync::Mutex<ConvEntry>>,
    span_id: u64,
    parent: Option<u64>,
    model: String,
    variant: String,
    label: String,
    started: Instant,
    start_offset: Duration,
    cpu_start: Option<Duration>,
    tokens_prompt: Option<u64>,
    tokens_completion: Option<u64>,
    attributes: BTreeMap<String, String>,
    otel: tracing::Span,
    finished: bool,
}

impl ConvSpanGuard {
    /// Attach token counts before finishing (token-billed models only).
    pub fn tokens(mut self, prompt: Option<u64>, completion: Option<u64>) -> Self {
        self.tokens_prompt = prompt;
        self.tokens_completion = completion;
        self
    }

    /// Record the span under the conversation's OTel root for the given
    /// future — the correct async instrumentation (no guard across await):
    /// `guard.run(engine_call_future).await`.
    pub async fn run<F: std::future::Future>(&mut self, fut: F) -> F::Output {
        fut.instrument(self.otel.clone()).await
    }

    /// Record a successful call.
    pub fn finish_ok(mut self) {
        self.record("ok");
    }

    /// Record a failed call (falls back to another model — the chain
    /// shows the resource spent on the failed attempt too).
    pub fn finish_err(mut self) {
        self.record("error");
    }

    fn record(&mut self, status: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        let duration = self.started.elapsed();
        let cpu = crate::observe::process_cpu_time()
            .zip(self.cpu_start)
            .map(|(now, start)| now.saturating_sub(start));
        let span = ConvSpan {
            span_id: self.span_id,
            parent_id: self.parent,
            model: self.model.clone(),
            variant: self.variant.clone(),
            label: self.label.clone(),
            start_ms: self.start_offset.as_millis() as u64,
            duration_ms: duration.as_millis() as u64,
            cpu_ms: cpu.map(|c| c.as_millis() as u64),
            status: status.to_string(),
            tokens_prompt: self.tokens_prompt,
            tokens_completion: self.tokens_completion,
            attributes: self.attributes.clone(),
        };
        if status == "error" {
            tracing::warn!(
                model = %self.model,
                duration_ms = span.duration_ms,
                "conversation model call failed"
            );
        }
        let mut e = self.entry.lock().expect("convtrace entry lock");
        e.inflight = e.inflight.saturating_sub(1);
        match status {
            "ok" => e.any_ok = true,
            _ => e.any_err = true,
        }
        if e.spans.len() < MAX_SPANS_PER_CONV {
            e.spans.push(span);
        }
    }
}

impl Drop for ConvSpanGuard {
    fn drop(&mut self) {
        self.record("ok");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_hub() -> ConvTraceHub {
        ConvTraceHub::new()
    }

    #[test]
    fn span_records_order_and_resources() {
        let hub = fresh_hub();
        let conv = hub.start("chat");
        let g = conv.span("vlm", "qwen3-vl-2b", "看图直答", vec![]);
        std::thread::sleep(Duration::from_millis(15));
        g.finish_ok();
        let g2 = conv.span("llm", "qwen3-4b", "本地应答", vec![]);
        std::thread::sleep(Duration::from_millis(5));
        g2.tokens(Some(120), Some(45)).finish_ok();
        conv.close();

        let d = hub.get(&conv.id()).expect("conversation present");
        assert_eq!(d.spans.len(), 2);
        // Call order: vlm first, llm second.
        assert_eq!(d.spans[0].model, "vlm");
        assert_eq!(d.spans[1].model, "llm");
        assert!(d.spans[0].duration_ms >= 10, "duration measured");
        assert_eq!(d.spans[1].tokens_prompt, Some(120));
        assert_eq!(d.spans[1].tokens_completion, Some(45));
        assert!(d.spans[0].start_ms <= d.spans[1].start_ms);
    }

    #[test]
    fn error_span_yields_partial_status() {
        let hub = fresh_hub();
        let conv = hub.start("voice");
        conv.span("cloud.chat", "gpt-x", "云端应答", vec![])
            .finish_err();
        conv.span("llm", "qwen3-4b", "本地回落", vec![]).finish_ok();
        conv.close();
        let d = hub.get(&conv.id()).unwrap();
        assert_eq!(d.spans.iter().filter(|s| s.status == "error").count(), 1);
        let s = hub.list(10).remove(0);
        assert_eq!(s.status, "partial");
        assert_eq!(s.models, vec!["cloud.chat".to_string(), "llm".to_string()]);
    }

    #[test]
    fn ring_caps_at_max_conversations() {
        let hub = fresh_hub();
        for _ in 0..(MAX_CONVERSATIONS + 10) {
            hub.start("chat").close();
        }
        assert_eq!(hub.list(500).len(), MAX_CONVERSATIONS);
    }

    #[test]
    fn span_cap_drops_new_spans_but_counts_them() {
        let hub = fresh_hub();
        let conv = hub.start("chat");
        for i in 0..(MAX_SPANS_PER_CONV + 5) {
            conv.span("llm", "v", &format!("s{i}"), vec![]).finish_ok();
        }
        let d = hub.get(&conv.id()).unwrap();
        assert_eq!(d.spans.len(), MAX_SPANS_PER_CONV);
    }

    #[test]
    fn dropped_guard_records_ok_without_tokens() {
        let hub = fresh_hub();
        let conv = hub.start("chat");
        {
            let _g = conv.span("decision", "laya", "意图决策", vec![]);
        } // dropped un-finished
        let d = hub.get(&conv.id()).unwrap();
        assert_eq!(d.spans.len(), 1);
        assert_eq!(d.spans[0].status, "ok");
        assert_eq!(d.spans[0].tokens_prompt, None);
    }

    #[test]
    fn attributes_carry_dialect_fields() {
        let hub = fresh_hub();
        let conv = hub.start("voice");
        conv.span(
            "decision",
            "laya",
            "意图决策",
            vec![("choice".into(), "answer".into())],
        )
        .finish_ok();
        conv.close();
        let d = hub.get(&conv.id()).unwrap();
        assert_eq!(
            d.spans[0].attributes.get("choice").map(String::as_str),
            Some("answer")
        );
    }

    #[test]
    fn turns_are_counted_per_conversation() {
        let hub = fresh_hub();
        let conv = hub.start("voice");
        conv.add_turn();
        conv.add_turn();
        conv.close();
        let s = hub.list(1).remove(0);
        assert_eq!(s.turns, 2);
    }

    #[tokio::test]
    async fn run_instruments_future_and_finishes_cleanly() {
        let hub = fresh_hub();
        let conv = hub.start("chat");
        let mut g = conv.span("llm", "qwen3-4b", "本地应答", vec![]);
        let out = g
            .run(async {
                tokio::task::yield_now().await;
                41
            })
            .await;
        assert_eq!(out, 41);
        g.tokens(None, Some(7)).finish_ok();
        let d = hub.get(&conv.id()).unwrap();
        assert_eq!(d.spans[0].tokens_completion, Some(7));
    }
}
