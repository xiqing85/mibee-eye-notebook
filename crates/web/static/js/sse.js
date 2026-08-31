// SSE event channel (SPEC §6): EventSource with capability-driven event
// subscriptions. EventSource reconnects natively; we only surface status.

import { store } from './store.js';
import { t } from './i18n.js';
import { badge } from './ui.js';

let source = null;
let handlers = {};

/// Connect /api/events and subscribe to every event type advertised by
/// capabilities.events. handlers: { <eventType>: (payload) => void }.
export function connectEvents(newHandlers) {
  handlers = { ...handlers, ...newHandlers };
  if (source) return;
  const types = (store.caps && store.caps.events) || [];
  if (types.length === 0) return;
  try {
    source = new EventSource('/api/events');
  } catch (_) {
    setEventsBadge('offline');
    return;
  }
  source.onopen = () => setEventsBadge('online');
  source.onerror = () => setEventsBadge('offline'); // auto-reconnect continues
  for (const type of types) {
    source.addEventListener(type, (ev) => {
      setEventsBadge('online');
      let payload = {};
      try { payload = JSON.parse(ev.data); } catch (_) { /* ignore malformed */ }
      const fn = handlers[type];
      if (fn) fn(payload);
    });
  }
}

export function disconnectEvents() {
  if (source) { source.close(); source = null; }
  setEventsBadge('offline');
}

function setEventsBadge(cls) {
  badge('events-badge', 'events-label', cls, t(cls === 'online' ? 'connected' : 'disconnected'));
}
