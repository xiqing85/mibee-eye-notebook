// Local LLM chat (SPEC appendix A #22, extension: chat): a floating
// panel that POSTs to /api/chat with the running history. The device
// answers from the local Qwen3 GGUF — no cloud.

import { api } from './api.js';
import { store, hasCap } from './store.js';
import { $, toast, setBtnLoading } from './ui.js';
import { t } from './i18n.js';

const MAX_HISTORY = 20;   // turns kept for context (device truncates anyway)
let history = [];         // [{role, content}]
let open = false;

export function initChat() {
  const fab = $('chat-fab');
  if (!fab) return;
  fab.addEventListener('click', toggleChat);
  const close = $('chat-close');
  if (close) close.addEventListener('click', toggleChat);
  const send = $('chat-send');
  if (send) send.addEventListener('click', sendChat);
  const input = $('chat-input');
  if (input) {
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.isComposing) sendChat();
    });
  }
  updateChatVisibility();
}

export function updateChatVisibility() {
  const fab = $('chat-fab');
  if (fab) fab.classList.toggle('hidden', !hasCap('chat'));
  if (!hasCap('chat')) {
    const panel = $('chat-panel');
    if (panel) panel.classList.add('hidden');
    open = false;
  }
}

function toggleChat() {
  const panel = $('chat-panel');
  if (!panel) return;
  open = !open;
  panel.classList.toggle('hidden', !open);
  if (open) {
    const input = $('chat-input');
    if (input) input.focus();
  }
}

async function sendChat() {
  const input = $('chat-input');
  const btn = $('chat-send');
  if (!input) return;
  const text = input.value.trim();
  if (!text) return;
  input.value = '';
  appendBubble('user', text);
  history.push({ role: 'user', content: text });
  setBtnLoading(btn, true);
  try {
    const res = await api.post('/api/chat', { text, history: history.slice(0, -1) });
    const reply = res.ok && res.data && res.data.reply ? res.data.reply : null;
    if (reply === null) {
      toast(t('chatFailed'), 'error');
      appendBubble('assistant', t('chatFailed'));
    } else {
      appendBubble('assistant', reply);
      history.push({ role: 'assistant', content: reply });
    }
  } finally {
    setBtnLoading(btn, false);
    if (history.length > MAX_HISTORY) history = history.slice(-MAX_HISTORY);
    const log = $('chat-log');
    if (log) log.scrollTop = log.scrollHeight;
  }
}

function appendBubble(role, text) {
  const log = $('chat-log');
  if (!log) return;
  const div = document.createElement('div');
  div.className = 'chat-bubble ' + (role === 'user' ? 'mine' : 'theirs');
  div.textContent = text;
  log.appendChild(div);
  log.scrollTop = log.scrollHeight;
}

/// SSE `chat_reply` (voice-driven replies, SPEC appendix A #22) — mirror
/// into the panel when it is open, toast otherwise.
export function handleChatReplyEvent(p) {
  if (!p || !p.reply) return;
  if (p.source !== 'voice') return;
  if (open) {
    history.push({ role: 'assistant', content: p.reply });
    appendBubble('assistant', p.reply);
  } else {
    toast(t('voiceReply', { s: p.reply }), 'info');
  }
}
