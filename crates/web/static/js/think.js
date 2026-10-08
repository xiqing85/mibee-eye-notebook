// Live thinking panel (SPEC §3.4 thinking entries + §6 `agent_step`).
// Two sections: the live agent timeline (tool calls running → done as
// they happen) and the latest turn's full internal call list. The live
// section clears itself when the matching `conversation` record lands —
// the finished turn then owns the display.

import { hasCap } from './store.js';
import { $ } from './ui.js';
import { t } from './i18n.js';
import { toolCard } from './chat.js';
import { setWaveformState } from './waveform.js';

let liveCount = 0;

export function initThink() {
  const card = $('think-card');
  if (card) card.classList.toggle('hidden', !hasCap('conversations'));
  const btn = $('think-live');
  if (btn) btn.classList.add('think-idle');
  // The chat's clear-all also empties both panel sections.
  window.addEventListener('mibee:conv-cleared', () => {
    handleTurnFinished(null);
    const turn = $('think-turn');
    if (turn) turn.innerHTML = '';
  });
}

/// SSE `agent_step` (SPEC §6): kind=tool drives the live timeline;
/// kind=phase flips the hero state (thinking/answering).
export function handleAgentStep(p) {
  if (!p || !hasCap('conversations')) return;
  if (p.kind === 'phase') {
    if (p.state === 'thinking') setWaveformState('thinking');
    if (p.state === 'answering') setWaveformState('answering');
    setActivity(p.state === 'thinking' ? t('agentActivityThink') : '');
    return;
  }
  if (p.kind !== 'tool') return;
  const box = $('think-live');
  if (!box) return;
  box.classList.remove('think-idle');
  if (p.state === 'running') {
    liveCount++;
    const row = document.createElement('div');
    row.className = 'think-live-row';
    row.setAttribute('data-tool', String(p.tool || ''));
    row.appendChild(toolCard({
      name: p.tool, args: p.args, state: 'running',
    }));
    box.appendChild(row);
    setActivity(t('agentActivityTool', { name: p.tool || '?' }));
  } else {
    // done | error — upgrade the matching running card in place.
    const rows = box.querySelectorAll('.think-live-row[data-tool="' + String(p.tool || '') + '"]');
    const row = rows.length ? rows[rows.length - 1] : null;
    const card = row ? row.querySelector('.chat-tool') : null;
    if (card && card.classList.contains('running')) {
      card.classList.remove('running');
      card.replaceWith(toolCard({
        name: p.tool, args: p.args, ok: p.state !== 'error',
        result: p.result, duration_ms: p.duration_ms,
        state: p.state === 'error' ? 'error' : undefined,
      }));
      setActivity('');
    } else {
      liveCount++;
      const fresh = document.createElement('div');
      fresh.className = 'think-live-row';
      fresh.setAttribute('data-tool', String(p.tool || ''));
      fresh.appendChild(toolCard({
        name: p.tool, args: p.args, ok: p.state !== 'error',
        result: p.result, duration_ms: p.duration_ms,
        state: p.state === 'error' ? 'error' : undefined,
      }));
      box.appendChild(fresh);
    }
  }
}

/// The latest finished turn's thinking entries (SPEC §3.4) as a
/// timeline. Called on `conversation` events and on view entry.
export function showTurnThinking(turn) {
  const box = $('think-turn');
  if (!box || !turn) return;
  box.innerHTML = '';
  const head = document.createElement('div');
  head.className = 'think-turn-head';
  const origin = document.createElement('span');
  origin.className = 'trace-origin ' + (turn.origin === 'voice' ? 'voice' : 'chat');
  origin.textContent = t(turn.origin === 'voice' ? 'traceOriginVoice' : 'traceOriginChat');
  head.appendChild(origin);
  const text = document.createElement('span');
  text.className = 'think-turn-q';
  text.textContent = turn.user_text || '';
  head.appendChild(text);
  box.appendChild(head);
  const thinking = Array.isArray(turn.thinking) ? turn.thinking : [];
  if (!thinking.length) {
    const empty = document.createElement('p');
    empty.className = 'record-empty';
    empty.textContent = t('thinkTurnEmpty');
    box.appendChild(empty);
    return;
  }
  for (const entry of thinking) {
    const row = document.createElement('div');
    row.className = 'think-step' + (entry.source === 'tool' ? ' tool' : '');
    const dot = document.createElement('span');
    dot.className = 'think-step-dot';
    row.appendChild(dot);
    const src = document.createElement('span');
    src.className = 'think-step-src mono';
    src.textContent = entry.source === 'tool'
      ? String(entry.model || 'tool')
      : String(entry.source || '?');
    row.appendChild(src);
    if (entry.note) {
      const note = document.createElement('span');
      note.className = 'think-step-note';
      note.textContent = entry.note;
      row.appendChild(note);
    }
    if (entry.duration_ms !== undefined && entry.duration_ms !== null) {
      const dur = document.createElement('span');
      dur.className = 'think-step-dur mono';
      dur.textContent = (entry.duration_ms / 1000).toFixed(1) + 's';
      row.appendChild(dur);
    }
    box.appendChild(row);
  }
}

/// A finished turn clears the live section (its record now owns the
/// display) and repopulates the turn timeline.
export function handleTurnFinished(turn) {
  if (!hasCap('conversations')) return;
  const box = $('think-live');
  if (box) {
    box.innerHTML = t('thinkLiveEmpty');
    box.classList.add('think-idle');
  }
  liveCount = 0;
  setActivity('');
  showTurnThinking(turn);
}

function setActivity(text) {
  const el = $('agent-activity');
  if (el) el.textContent = text || '';
}
