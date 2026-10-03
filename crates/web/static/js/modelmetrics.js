// Per-model resource usage from the public /metrics surface (SPEC v1
// §3.3 + appendix A #39): parses the mibee_model_* families into
// per-model cards — calls, latency, CPU time, errors, tokens.

import { store } from './store.js';
import { $, esc } from './ui.js';
import { t } from './i18n.js';

export function modelMetricsCap() {
  const obs = (store.caps && store.caps.observability) || {};
  return !!obs.model_metrics;
}

// ─── Prometheus text parsing (subset: NAME{label="v",...} VALUE) ──────

function parseSamples(text) {
  const samples = [];
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    const m = line.match(/^([a-zA-Z_][a-zA-Z0-9_]*)\{([^}]*)\}\s+([-+0-9.eE]+)$/);
    if (m) {
      samples.push({ name: m[1], labels: parseLabels(m[2]), value: parseFloat(m[3]) });
      continue;
    }
    const m2 = line.match(/^([a-zA-Z_][a-zA-Z0-9_]*)\s+([-+0-9.eE]+)$/);
    if (m2) samples.push({ name: m2[1], labels: {}, value: parseFloat(m2[2]) });
  }
  return samples;
}

function parseLabels(s) {
  const labels = {};
  const re = /([a-zA-Z_][a-zA-Z0-9_]*)="((?:[^"\\]|\\.)*)"/g;
  let m;
  while ((m = re.exec(s))) labels[m[1]] = m[2].replace(/\\(.)/g, '$1');
  return labels;
}

/// Aggregate mibee_model_* samples into [{model, variant, calls, errors,
/// seconds, cpu, tokensPrompt, tokensCompletion, inflight}].
export function aggregateModelMetrics(text) {
  const byKey = new Map();
  const ensure = (model, variant) => {
    const key = model + '\u0000' + (variant || '');
    let row = byKey.get(key);
    if (!row) {
      row = {
        model, variant: variant || '', calls: 0, errors: 0, seconds: 0,
        cpu: 0, tokensPrompt: 0, tokensCompletion: 0,
      };
      byKey.set(key, row);
    }
    return row;
  };
  const inflight = new Map();
  for (const s of parseSamples(text)) {
    const l = s.labels;
    if (s.name === 'mibee_model_inferences_total' && l.model) {
      ensure(l.model, l.variant).calls = s.value;
    } else if (s.name === 'mibee_model_errors_total' && l.model) {
      ensure(l.model, l.variant).errors = s.value;
    } else if (s.name === 'mibee_model_inference_seconds_sum' && l.model) {
      ensure(l.model, l.variant).seconds = s.value;
    } else if (s.name === 'mibee_model_cpu_seconds_sum' && l.model) {
      ensure(l.model, l.variant).cpu = s.value;
    } else if (s.name === 'mibee_model_tokens_total' && l.model) {
      const row = ensure(l.model, l.variant);
      if (l.kind === 'prompt') row.tokensPrompt = s.value;
      if (l.kind === 'completion') row.tokensCompletion = s.value;
    } else if (s.name === 'mibee_model_inflight' && l.model) {
      inflight.set(l.model, s.value);
    }
  }
  const rows = [...byKey.values()].filter((r) => r.calls > 0 || r.errors > 0 || r.seconds > 0);
  for (const r of rows) r.inflight = inflight.get(r.model) || 0;
  rows.sort((a, b) => (a.model + a.variant).localeCompare(b.model + b.variant));
  return rows;
}

// ─── Rendering ────────────────────────────────────────────────────────

function fmtMs(seconds) {
  if (!seconds) return '-';
  if (seconds < 1) return (seconds * 1000).toFixed(0) + ' ms';
  return seconds.toFixed(1) + ' s';
}

function fmtTokens(n) {
  if (!n) return '';
  return n >= 10000 ? (n / 1000).toFixed(1) + 'k' : String(Math.round(n));
}

export function renderModelRows(rows) {
  const box = $('obs-models');
  if (!box) return;
  if (!rows.length) {
    box.innerHTML = '<p class="record-empty">' + esc(t('modelsMetricsEmpty')) + '</p>';
    return;
  }
  box.innerHTML = rows.map((r) => {
    const avg = r.calls ? r.seconds / r.calls : 0;
    const errPct = r.calls ? (r.errors / r.calls) * 100 : 0;
    const inflight = r.inflight
      ? '<span class="state-pill on">' + esc(t('modelInflight')) + ' ' + r.inflight + '</span>'
      : '';
    return '<div class="mm-row" data-model="' + esc(r.model) + '">' +
      '<div class="mm-head"><span class="mm-model mono">' + esc(r.model) + '</span>' +
      (r.variant ? '<span class="mm-variant" title="' + esc(r.variant) + '">' + esc(r.variant) + '</span>' : '') + inflight + '</div>' +
      '<div class="mm-stats">' +
      '<div class="mm-stat"><span class="mm-stat-label">' + esc(t('modelCalls')) + '</span><span class="mm-stat-val mono">' + r.calls + '</span></div>' +
      '<div class="mm-stat"><span class="mm-stat-label">' + esc(t('modelAvgLatency')) + '</span><span class="mm-stat-val mono">' + esc(fmtMs(avg)) + '</span></div>' +
      '<div class="mm-stat"><span class="mm-stat-label">' + esc(t('modelCpuTotal')) + '</span><span class="mm-stat-val mono">' + esc(fmtMs(r.cpu)) + '</span></div>' +
      '<div class="mm-stat"><span class="mm-stat-label">' + esc(t('modelErrors')) + '</span><span class="mm-stat-val mono' + (r.errors ? ' mm-err' : '') + '">' + Math.round(r.errors) + (r.errors ? ' (' + errPct.toFixed(0) + '%)' : '') + '</span></div>' +
      ((r.tokensPrompt || r.tokensCompletion)
        ? '<div class="mm-stat"><span class="mm-stat-label">tokens</span><span class="mm-stat-val mono">' +
          esc(fmtTokens(r.tokensPrompt) + '↑/' + fmtTokens(r.tokensCompletion) + '↓') + '</span></div>'
        : '') +
      '</div></div>';
  }).join('');
}

let timer = null;

export async function pollModelMetrics() {
  try {
    const res = await fetch('/metrics', { credentials: 'same-origin' });
    if (!res.ok) return;
    renderModelRows(aggregateModelMetrics(await res.text()));
  } catch (_) { /* scrape failures are silent — next tick retries */ }
}

export function initModelMetrics() {
  const card = $('obs-models-card');
  if (card) card.classList.toggle('hidden', !modelMetricsCap());
}

export function startModelMetricsPolling() {
  if (timer) return;
  timer = setInterval(() => {
    if (!modelMetricsCap()) return;
    const pane = $('sys-status');
    if (pane && pane.classList.contains('active')) pollModelMetrics();
  }, 5000);
}
