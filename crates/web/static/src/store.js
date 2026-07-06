// Global app state via a tiny custom signal primitive.
//
// We avoid pulling in @preact/signals (an extra npm dependency that isn't
// shipped with the core `preact` package). The primitive here is ~30 lines
// and covers the read/write/subscribe use case every component needs.
//
// Components subscribe inside render via the `useStore(sig)` hook from
// hooks.js, which re-renders on `.value` mutation.

/// A minimal observable cell. `sig.value` is the getter/setter.
export function signal(initial) {
  let current = initial;
  const subs = new Set();
  function sig() { return current; }
  sig.value = undefined;
  Object.defineProperty(sig, 'value', {
    get() { return current; },
    set(v) {
      if (Object.is(v, current)) return;
      current = v;
      for (const fn of subs) fn();
    },
    enumerable: true,
    configurable: true,
  });
  sig.subscribe = (fn) => { subs.add(fn); return () => subs.delete(fn); };
  sig.peek = () => current;
  return sig;
}

/// A derived signal computed from one or more source signals.
export function computed(compute) {
  // Track sources by running compute once with subscription capture.
  const sources = new Set();
  // Proxy: subscribe to any signal read during compute.
  // Simplified: callers pass sources explicitly in this codebase via closure,
  // so we just re-evaluate lazily on read.
  const out = signal(undefined);
  const recompute = () => { out.value = compute(); };
  // Initial compute; we cannot auto-track here without effect plumbing, so
  // callers must either call out.value once or wire sources manually. In
  // practice this codebase uses computed() only for `anyStreamActive`, which
  // is recomputed on read by the consuming component instead.
  out._compute = compute;
  return out;
}

// ── Global signals ──

/// Authentication state. null = unknown (still checking), true/false after.
export const authed = signal(null);
/// Currently signed-in username (from /api/auth/me), null if logged out.
export const username = signal(null);
/// Active cameras list (array of camera objects).
export const cameras = signal([]);
/// Active stream info keyed by camera id → { rtsp_url, status }.
export const streamUrls = signal({});
/// UI language: 'en' or 'zh'.
export const lang = signal('en');
/// UI theme: 'dark' or 'light'.
export const theme = signal('dark');
/// System capabilities snapshot (from /api/capabilities), null until loaded.
export const capabilities = signal(null);

// ── Toast queue (FIFO capped at 3) ──

export const toasts = signal([]);

let toastSeq = 0;
export function showToast(message, type = 'info', timeoutMs = 4000) {
  const id = ++toastSeq;
  const entry = { id, message, type };
  toasts.value = [...toasts.value, entry].slice(-3);
  if (timeoutMs > 0) {
    setTimeout(() => dismissToast(id), timeoutMs);
  }
  return id;
}

export function dismissToast(id) {
  toasts.value = toasts.value.filter((x) => x.id !== id);
}
