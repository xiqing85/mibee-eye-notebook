// Auth view: first-boot setup / login / logout (SPEC §2).
//
// The auth state machine is driven entirely by GET /api/auth/me:
//   200 → already signed in (session cookie alive)
//   401 → login form
//   503 setup_required → first-boot setup form

import { api } from './api.js';
import { store } from './store.js';
import { $, setBtnLoading } from './ui.js';
import { t } from './i18n.js';

export const AuthState = { SIGNED_IN: 'in', LOGIN: 'login', SETUP: 'setup' };

export async function detectAuthState() {
  const r = await api.get('/api/auth/me');
  if (r.ok) {
    store.user = r.data;
    return AuthState.SIGNED_IN;
  }
  if (r.status === 503 && r.error === 'setup_required') return AuthState.SETUP;
  return AuthState.LOGIN;
}

export function setAuthMode(mode) {
  const setup = mode === AuthState.SETUP;
  $('auth-username-field').classList.toggle('hidden', !setup);
  $('login-password2-field').classList.toggle('hidden', !setup);
  const hint = $('setup-hint');
  if (hint) hint.classList.toggle('hidden', !setup);
  const btn = document.querySelector('#login-form button[type=submit]');
  if (btn) btn.textContent = setup ? t('setupBtn') : t('loginBtn');
  const pwLabel = document.querySelector('label[for=login-password]');
  if (pwLabel) pwLabel.textContent = setup ? t('newPassword') : t('password');
  const err = $('login-error');
  if (err) err.classList.add('hidden');
}

export function initAuth(onAuthenticated) {
  $('login-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const username = $('login-username').value.trim();
    const password = $('login-password').value;
    const errEl = $('login-error');
    errEl.classList.add('hidden');
    const btn = document.querySelector('#login-form button[type=submit]');

    const isSetup = !$('auth-username-field').classList.contains('hidden');
    if (isSetup) {
      const confirm2 = $('login-password2') ? $('login-password2').value : password;
      if (!username || password.length < 8) {
        errEl.textContent = t('passwordTooShort'); errEl.classList.remove('hidden'); return;
      }
      if (password !== confirm2) {
        errEl.textContent = t('passwordMismatch'); errEl.classList.remove('hidden'); return;
      }
      setBtnLoading(btn, true);
      try {
        const r = await api.post('/api/auth/setup', { username, password });
        if (!r.ok) { showAuthError(r); return; }
        store.user = r.data || { username };
        onAuthenticated();
      } finally {
        setBtnLoading(btn, false);
      }
      return;
    }

    setBtnLoading(btn, true);
    try {
      const r = await api.post('/api/auth/login', { username, password });
      if (!r.ok) { showAuthError(r); return; }
      store.user = r.data || { username };
      onAuthenticated();
    } finally {
      setBtnLoading(btn, false);
    }
  });
}

function showAuthError(r) {
  const errEl = $('login-error');
  if (!errEl) return;
  if (r.status === 401) errEl.textContent = t('loginInvalid');
  else if (r.status === 429) errEl.textContent = t('rateLimited');
  else errEl.textContent = r.message || t('fetchError');
  errEl.classList.remove('hidden');
}

export async function handleLogout(onLoggedOut) {
  await api.post('/api/auth/logout');
  store.user = null;
  onLoggedOut();
}
