// Thin API client wrapping fetch with auth/CSRF/401 handling.
//
// All endpoints live under /api. State-changing requests (POST/PUT/DELETE/
// PATCH) automatically attach the X-CSRF-Token header read from the
// csrf-token cookie (double-submit pattern). A 401 anywhere triggers a
// single `onSessionExpired` callback so the UI can redirect to login.

let sessionExpiredHandler = null;

/// Register a callback invoked once when any request returns 401.
export function onSessionExpired(fn) {
  sessionExpiredHandler = fn;
}

/// Read a named cookie value, or null if absent.
function getCookie(name) {
  for (const part of document.cookie.split(';')) {
    const c = part.trim();
    if (c.startsWith(name + '=')) {
      return c.slice(name.length + 1);
    }
  }
  return null;
}

/// Core request helper. Returns { ok, status, data }.
export async function request(method, path, body) {
  const opts = { method, headers: {}, credentials: 'same-origin' };
  if (body !== undefined && body !== null) {
    opts.headers['Content-Type'] = 'application/json';
    opts.body = JSON.stringify(body);
  }
  const isStateChanging = /^(POST|PUT|DELETE|PATCH)$/.test(method);
  // Auth endpoints (login/setup/logout) are CSRF-exempt server-side.
  const isAuthEndpoint =
    path === '/api/auth/login' || path === '/api/auth/setup' || path === '/api/auth/logout';
  if (isStateChanging && !isAuthEndpoint) {
    const csrf = getCookie('csrf-token');
    if (csrf) opts.headers['X-CSRF-Token'] = csrf;
  }

  let res;
  try {
    res = await fetch(path, opts);
  } catch (e) {
    return { ok: false, status: 0, data: null, error: 'network' };
  }

  if (res.status === 401 && sessionExpiredHandler) {
    const fn = sessionExpiredHandler;
    sessionExpiredHandler = null; // fire once
    fn();
  }

  const ct = res.headers.get('content-type') || '';
  let data = null;
  if (ct.includes('application/json')) {
    data = await res.json().catch(() => null);
  }
  return { ok: res.ok, status: res.status, data };
}

export const api = {
  get: (p) => request('GET', p),
  post: (p, b) => request('POST', p, b),
  put: (p, b) => request('PUT', p, b),
  del: (p) => request('DELETE', p),
  request,
};
