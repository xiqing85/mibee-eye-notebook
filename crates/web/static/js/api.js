// API client: cookie session + CSRF double-submit + spec envelope unwrap.
//
// SPEC v1: success {"ok":true,"data":...}, failure {"ok":false,"error","message"}.
// The session cookie is sent automatically (same-origin). State-changing
// requests attach X-CSRF-Token read from the csrf-token cookie, except on
// the CSRF-exempt auth endpoints (SPEC §2).

import { store } from './store.js';

let sessionExpiredHandler = null;

/// Register a callback fired once when any request returns 401 — the UI
/// uses it to return to the login view.
export function onSessionExpired(fn) {
  sessionExpiredHandler = fn;
}

export function getCookie(name) {
  for (const part of document.cookie.split(';')) {
    const c = part.trim();
    if (c.startsWith(name + '=')) return c.slice(name.length + 1);
  }
  return null;
}

const AUTH_EXEMPT = new Set(['/api/auth/login', '/api/auth/setup', '/api/auth/logout']);

/// Core request helper. Returns { ok, status, data, error, message }.
/// `data` is the unwrapped envelope payload on success.
export async function request(method, path, body) {
  const opts = { method, headers: {}, credentials: 'same-origin' };
  if (body !== undefined && body !== null) {
    opts.headers['Content-Type'] = 'application/json';
    opts.body = JSON.stringify(body);
  }
  const isWrite = /^(POST|PUT|DELETE|PATCH)$/.test(method);
  if (isWrite && !AUTH_EXEMPT.has(path)) {
    const csrf = getCookie('csrf-token');
    if (csrf) opts.headers['X-CSRF-Token'] = csrf;
  }

  let res;
  try {
    res = await fetch(path, opts);
  } catch (_) {
    return { ok: false, status: 0, data: null, error: 'network', message: null };
  }

  if (res.status === 401) {
    // A session that died mid-flight (restart, expiry) — surface it once.
    if (sessionExpiredHandler && path !== '/api/auth/login') {
      const fn = sessionExpiredHandler;
      sessionExpiredHandler = null;
      fn();
    }
  }

  const ct = res.headers.get('content-type') || '';
  if (!ct.includes('application/json')) {
    return { ok: res.ok, status: res.status, data: null, error: res.ok ? null : 'http_' + res.status, message: null };
  }
  const bodyJson = await res.json().catch(() => null);
  if (bodyJson && bodyJson.ok === true) {
    return { ok: true, status: res.status, data: bodyJson.data, error: null, message: null };
  }
  return {
    ok: false,
    status: res.status,
    data: bodyJson && bodyJson.ok === false ? null : bodyJson,
    error: (bodyJson && bodyJson.error) || ('http_' + res.status),
    message: bodyJson && bodyJson.message,
  };
}

export const api = {
  get: (p) => request('GET', p),
  post: (p, b) => request('POST', p, b),
  put: (p, b) => request('PUT', p, b),
  del: (p) => request('DELETE', p),
  request,
};

/// Reload capabilities into the store. Called once after login and after
/// any config write that might toggle a feature.
export async function refreshCapabilities() {
  const r = await api.get('/api/capabilities');
  if (r.ok) store.caps = r.data;
  return r;
}
