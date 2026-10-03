// Conversation model-call chains (SPEC v1 §3.3): recent conversations,
// their model spans in call order, and a waterfall visualization of the
// resources each link consumed (duration bar + CPU delta + tokens).

import { api } from './api.js';
import { store } from './store.js';
import { $, esc } from './ui.js';
import { t } from './i18n.js';

let selectedId = null;
let timer = null;

export function tracesCap() {
  const obs = (store.caps && store.caps.observability) || {};
  return !!obs.traces;
}

const MODEL_HUES = {};
function modelColor(model) {
  if (!(model in MODEL_HUES)) {
    let h = 0;
    for (const ch of model) h = (h * 31 + ch.charCodeAt(0)) % 360;
    MODEL_HUES[model] = h;
  }
  return 'hsl(' + MODEL_HUES[model] + ' 62% 48%)';
}

function fmtDur(ms) {
  if (ms === undefined || ms === null) return '-';
  if (ms < 1000) return Math.round(ms) + ' ms';
  return (ms / 1000).toFixed(1) + ' s';
}

function fmtClock(ms) {
  const d = new Date(Number(ms) || 0);
  if (isNaN(d.getTime())) return '-';
  const p = (n) => String(n).padStart(2, '0');
  return p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
}

function statusClass(status) {
  if (status === 'ok') return 'on';
  if (status === 'partial') return 'warn';
  return 'off';
}

function originLabel(origin) {
  return t(origin === 'voice' ? 'traceOriginVoice' : 'traceOriginChat');
}

// ─── List ─────────────────────────────────────────────────────────────

export function renderTraceList(convs) {
  const box = $('traces-list');
  if (!box) return;
  if (!convs.length) {
    box.innerHTML = '<p class="record-empty">' + esc(t('tracesEmpty')) + '</p>';
    return;
  }
  box.innerHTML = convs.map((c) =>
    '<button type="button" class="trace-item' + (c.id === selectedId ? ' selected' : '') +
    '" data-trace="' + esc(c.id) + '">' +
    '<span class="trace-origin ' + esc(c.origin) + '">' + esc(originLabel(c.origin)) + '</span>' +
    '<span class="trace-meta"><span class="mono">' + esc(fmtClock(c.started_at_ms)) + '</span>' +
    '<span class="trace-models mono">' + esc((c.models || []).join(' → ')) + '</span></span>' +
    '<span class="trace-side"><span class="mono">' + esc(fmtDur(c.duration_ms)) + '</span>' +
    '<span class="state-pill ' + statusClass(c.status) + '">' + esc(c.status) + '</span></span>' +
    '</button>'
  ).join('');
  [...box.querySelectorAll('.trace-item')].forEach((el) => {
    el.addEventListener('click', () => {
      selectedId = el.getAttribute('data-trace');
      refreshTraces();
    });
  });
}

// ─── Waterfall ────────────────────────────────────────────────────────

/// SVG waterfall: one row per span, ordered by start_ms (call order —
/// sequence number circled at the left); the bar spans start→end against
/// the conversation total; duration/CPU/tokens annotate each link.
export function renderTraceDetail(detail) {
  const box = $('trace-detail');
  if (!box) return;
  if (!detail) {
    box.innerHTML = '';
    return;
  }
  const spans = (detail.spans || []).slice().sort((a, b) => a.start_ms - b.start_ms);
  const total = Math.max(
    detail.duration_ms || 0,
    ...spans.map((s) => s.start_ms + s.duration_ms),
    1
  );
  const W = 620, LABEL = 168, ROW = 30, HEAD = 26;
  const H = HEAD + spans.length * ROW + 8;
  const scale = (W - LABEL - 14) / total;
  const rows = spans.map((s, i) => {
    const y = HEAD + i * ROW;
    const x = LABEL + s.start_ms * scale;
    const w = Math.max(s.duration_ms * scale, 3);
    const color = modelColor(s.model);
    const seq = '①②③④⑤⑥⑦⑧⑨⑩⑪⑫⑬⑭⑮'[i] || String(i + 1);
    const tokens = (s.tokens_prompt !== null && s.tokens_prompt !== undefined) ||
      (s.tokens_completion !== null && s.tokens_completion !== undefined)
      ? ' · ' + (s.tokens_prompt || 0) + '↑' + (s.tokens_completion || 0) + '↓'
      : '';
    const cpu = s.cpu_ms !== null && s.cpu_ms !== undefined ? ' · cpu ' + fmtDur(s.cpu_ms) : '';
    const label = s.model + (s.variant ? ':' + s.variant : '');
    const errCls = s.status === 'error' ? ' trace-bar-err' : '';
    return '<g transform="translate(0 ' + y + ')">' +
      '<text x="6" y="12" class="trace-seq">' + seq + '</text>' +
      '<text x="26" y="12" class="trace-label">' + esc(label) + '</text>' +
      '<text x="' + (LABEL - 6) + '" y="12" text-anchor="end" class="trace-note">' + esc(fmtDur(s.duration_ms)) + '</text>' +
      '<rect x="' + x + '" y="1" width="' + w + '" height="14" rx="3" fill="' + color + '" class="trace-bar' + errCls + '"/>' +
      '<text x="' + (x + w + 6) + '" y="12" class="trace-note">' + esc(cpu + tokens) + '</text>' +
      '</g>';
  }).join('');
  const axis = '<line x1="' + LABEL + '" y1="' + (HEAD - 6) + '" x2="' + (W - 10) + '" y2="' + (HEAD - 6) + '" class="trace-axis"/>' +
    '<text x="' + LABEL + '" y="' + (HEAD - 10) + '" class="trace-note">0</text>' +
    '<text x="' + (W - 10) + '" y="' + (HEAD - 10) + '" text-anchor="end" class="trace-note">' + esc(fmtDur(total)) + '</text>';
  const head = '<div class="trace-detail-head">' +
    '<span class="trace-origin ' + esc(detail.origin) + '">' + esc(originLabel(detail.origin)) + '</span>' +
    '<span class="mono">' + esc(fmtClock(detail.started_at_ms)) + '</span>' +
    '<span>' + esc(String(detail.turns)) + ' ' + esc(t('traceTurns')) + '</span>' +
    '<span class="mono">' + esc(fmtDur(detail.duration_ms)) + '</span>' +
    '<span class="state-pill ' + statusClass(detail.status || 'ok') + '">' + esc(detail.status || 'ok') + '</span>' +
    '</div>';
  box.innerHTML = head +
    '<svg viewBox="0 0 ' + W + ' ' + H + '" class="trace-waterfall" role="img" aria-label="model call chain">' +
    axis + rows + '</svg>';
}

// ─── Fetch cycle ──────────────────────────────────────────────────────

export async function refreshTraces() {
  if (!tracesCap()) return;
  const r = await api.get('/api/traces/conversations?limit=20');
  if (!r.ok) return;
  const convs = (r.data && r.data.conversations) || [];
  if (!selectedId && convs.length) selectedId = convs[0].id;
  if (selectedId && !convs.some((c) => c.id === selectedId)) {
    selectedId = convs.length ? convs[0].id : null;
  }
  renderTraceList(convs);
  if (selectedId) {
    const d = await api.get('/api/traces/conversations/' + encodeURIComponent(selectedId));
    renderTraceDetail(d.ok ? d.data : null);
  } else {
    renderTraceDetail(null);
  }
}

export function initTraces() {
  const card = $('traces-card');
  if (card) card.classList.toggle('hidden', !tracesCap());
  const btn = $('traces-refresh');
  if (btn) btn.addEventListener('click', () => refreshTraces());
}

export function startTracesPolling() {
  if (timer) return;
  timer = setInterval(() => {
    if (!tracesCap()) return;
    const view = $('view-assistant');
    if (view && view.classList.contains('active')) refreshTraces();
  }, 5000);
}
