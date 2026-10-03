// Local LLM chat (SPEC appendix A #22, extension: chat). Since the 2026-10
// redesign the chat lives inline in the Assistant view (it used to be a
// floating FAB + panel). POSTs to /api/chat with the running history; the
// device answers from the local Qwen3 GGUF unless a cloud provider is on.
//
// Grounded chat (SPEC appendix A #29): every reply carries a `grounded`
// mode — "scene" (the device injected live camera context into the
// answer) shows an eye badge; the 👁 toggle requests "vlm" (direct
// frame Q&A through the vision model — honest slow, tens of seconds
// on CPU devices), fail-open back to scene grounding.

import { api } from './api.js';
import { store, hasCap } from './store.js';
import { $, toast, setBtnLoading } from './ui.js';
import { t } from './i18n.js';

const MAX_HISTORY = 20;   // turns kept for context (device truncates anyway)
let history = [];         // [{role, content}]
let vision = localStorage.getItem('mibee_chat_vision') === '1'; // explicit VLM Q&A

export function initChat() {
  const send = $('chat-send');
  if (send) send.addEventListener('click', sendChat);
  const eye = $('chat-vision');
  if (eye) eye.addEventListener('click', () => {
    vision = !vision;
    localStorage.setItem('mibee_chat_vision', vision ? '1' : '0');
    updateVisionButton();
  });
  const input = $('chat-input');
  if (input) {
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.isComposing) sendChat();
    });
  }
  updateVisionButton();
  updateChatVisibility();
}

function updateVisionButton() {
  const eye = $('chat-vision');
  if (!eye) return;
  const show = hasCap('chat') && hasCap('vlm');
  eye.classList.toggle('hidden', !show);
  eye.classList.toggle('on', vision);
  eye.setAttribute('aria-pressed', String(vision));
}

export function updateChatVisibility() {
  if (!hasCap('chat')) {
    const card = $('chat-log');
    if (card) card.closest('.chat-card')?.classList.add('hidden');
  }
  updateVisionButton();
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
  const useVision = vision && hasCap('vlm');
  if (useVision) toast(t('chatVisionSlow'), 'info');
  try {
    const res = await api.post('/api/chat', {
      text,
      history: history.slice(0, -1),
      vision: useVision,
    });
    const reply = res.ok && res.data && res.data.reply ? res.data.reply : null;
    if (reply === null) {
      toast(t('chatFailed'), 'error');
      appendBubble('assistant', t('chatFailed'));
    } else {
      appendBubble('assistant', reply, res.data.grounded);
      history.push({ role: 'assistant', content: reply });
    }
  } finally {
    setBtnLoading(btn, false);
    if (history.length > MAX_HISTORY) history = history.slice(-MAX_HISTORY);
    const log = $('chat-log');
    if (log) log.scrollTop = log.scrollHeight;
  }
}

function appendBubble(role, text, grounded) {
  const log = $('chat-log');
  if (!log) return;
  const div = document.createElement('div');
  div.className = 'chat-bubble ' + (role === 'user' ? 'mine' : 'theirs');
  div.textContent = text;
  if (role === 'assistant' && grounded === 'vlm') {
    const badge = document.createElement('span');
    badge.className = 'chat-badge vlm';
    badge.textContent = t('chatGroundedVlm');
    badge.title = t('chatVisionTitle');
    div.appendChild(badge);
  } else if (role === 'assistant' && grounded === 'scene') {
    const badge = document.createElement('span');
    badge.className = 'chat-badge scene';
    badge.textContent = t('chatGroundedScene');
    badge.title = t('chatGroundedSceneTitle');
    div.appendChild(badge);
  }
  log.appendChild(div);
  log.scrollTop = log.scrollHeight;
}

/// SSE `chat_reply` (voice-driven replies, SPEC appendix A #22/#29) —
/// mirror into the Assistant chat when that view is open, toast otherwise.
export function handleChatReplyEvent(p) {
  if (!p || !p.reply) return;
  if (p.source !== 'voice') return;
  if (store.view === 'assistant') {
    history.push({ role: 'assistant', content: p.reply });
    appendBubble('assistant', p.reply, p.grounded);
  } else {
    toast(t('voiceReply', { s: p.reply }), 'info');
  }
}
