// Conversation records (SPEC v1 §3.4): the human-readable log of every
// dialogue turn — the heard/input text, the internal "thinking" entries
// (one per internal model call or routing decision, including failed
// fallback legs), and the AI reply with the engine that produced it.
// Voice turns happen away from the browser; this card is where they
// become visible.

import { api } from './api.js';
import { store } from './store.js';
import { $, esc } from './ui.js';
import { t } from './i18n.js';

let turns = [];
const expanded = new Set();

export function convLogCap() {
  return !!(store.caps && store.caps.conversations);
}

function fmtClock(ms) {
  const d = new Date(Number(ms) || 0);
  if (isNaN(d.getTime())) return '-';
  const p = (n) => String(n).padStart(2, '0');
  return p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
}

function fmtDur(ms) {
  if (ms === undefined || ms === null) return '';
  if (ms < 1000) return Math.round(ms) + ' ms';
  return (ms / 1000).toFixed(1) + ' s';
}

function originLabel(origin) {
  return t(origin === 'voice' ? 'traceOriginVoice' : 'traceOriginChat');
}

function userInputLabel(origin) {
  return t(origin === 'voice' ? 'convHeard' : 'convAsked');
}

function renderTurn(turn) {
  const id = String(turn.id);
  const hasReply = turn.reply_text !== null && turn.reply_text !== undefined && turn.reply_text !== '';
  const thinking = turn.thinking || [];
  const isOpen = expanded.has(id);
  const thinkRows = thinking.map((e) =>
    '<div class="conv-think-entry">' +
    '<span class="conv-think-src mono">' + esc(e.source + (e.model ? ':' + e.model : '')) + '</span>' +
    '<span class="conv-think-note">' + esc(e.note || '') + '</span>' +
    '<span class="conv-think-dur mono">' + esc(fmtDur(e.duration_ms)) + '</span>' +
    '</div>'
  ).join('');
  return '<div class="conv-turn" data-conv="' + esc(id) + '">' +
    '<div class="conv-head">' +
    '<span class="trace-origin ' + esc(turn.origin === 'voice' ? 'voice' : 'chat') + '">' + esc(originLabel(turn.origin)) + '</span>' +
    '<span class="mono">' + esc(fmtClock(turn.started_ms)) + '</span>' +
    (hasReply
      ? '<span class="state-pill on">' + esc(turn.engine || '?') + '</span>'
      : '<span class="state-pill off">' + esc(t('convNoReply')) + '</span>') +
    '</div>' +
    '<div class="conv-user"><span class="conv-label">' + esc(userInputLabel(turn.origin)) + '</span>' +
    esc(turn.user_text || '') + '</div>' +
    (hasReply
      ? '<div class="conv-reply"><span class="conv-label">' + esc(t('convReply')) + '</span>' +
        esc(turn.reply_text) + '</div>'
      : '') +
    (thinking.length
      ? '<button type="button" class="conv-thinking-toggle btn-small" data-conv-toggle="' + esc(id) + '">' +
        esc((isOpen ? t('convHideThinking') : t('convThinking')) + ' (' + thinking.length + ')') + '</button>' +
        (isOpen ? '<div class="conv-thinking">' + thinkRows + '</div>' : '')
      : '') +
    '</div>';
}

export function renderConvLog() {
  const box = $('convlog-list');
  if (!box) return;
  if (!turns.length) {
    box.innerHTML = '<p class="record-empty">' + esc(t('convEmpty')) + '</p>';
    return;
  }
  box.innerHTML = turns.map(renderTurn).join('');
  [...box.querySelectorAll('[data-conv-toggle]')].forEach((btn) => {
    btn.addEventListener('click', () => {
      const id = btn.getAttribute('data-conv-toggle');
      if (expanded.has(id)) expanded.delete(id);
      else expanded.add(id);
      renderConvLog();
    });
  });
}

export async function refreshConvLog() {
  if (!convLogCap()) return;
  const r = await api.get('/api/conversations?limit=50');
  if (!r.ok) return;
  turns = (r.data && r.data.conversations) || [];
  renderConvLog();
}

// SSE `conversation` (SPEC §3.4): a turn just completed — refresh the
// list when the assistant page is open so the new turn (and its
// thinking) appears live.
export function handleConversationEvent() {
  if (!convLogCap()) return;
  const view = $('view-assistant');
  if (view && view.classList.contains('active')) refreshConvLog();
}

export function initConvLog() {
  const card = $('convlog-card');
  if (card) card.classList.toggle('hidden', !convLogCap());
  const btn = $('convlog-refresh');
  if (btn) btn.addEventListener('click', () => refreshConvLog());
}
