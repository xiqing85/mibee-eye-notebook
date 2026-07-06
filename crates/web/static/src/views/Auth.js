// Auth views: Login + first-run Setup.
import { h } from 'preact';
import { useState } from 'preact/hooks';
import { t } from '../app.jsx';
import { api } from '../api.js';
import { authed, showToast } from '../store.js';

export function Login({ onAuthed }) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  async function submit(e) {
    e.preventDefault();
    if (!username || !password) { setError(t('login.error_empty')); return; }
    setBusy(true); setError('');
    const res = await api.post('/api/auth/login', { username, password });
    setBusy(false);
    if (res.ok) {
      showToast(t('login.signed_in'), 'success');
      authed.value = true;
      onAuthed();
    } else {
      setError(t('login.error_failed'));
    }
  }

  return (
    <section class="page auth-page">
      <div class="auth-card">
        <div class="brand-lg">mibee-rec</div>
        <h1>{t('login.title')}</h1>
        <p class="text-muted">{t('login.subtitle')}</p>
        {error && <div class="alert alert-error">{error}</div>}
        <form onSubmit={submit}>
          <div class="form-group">
            <label>{t('login.username')}</label>
            <input type="text" value={username} onInput={(e) => setUsername(e.target.value)}
                   placeholder={t('login.username_ph')} required autocomplete="username" />
          </div>
          <div class="form-group">
            <label>{t('login.password')}</label>
            <input type="password" value={password} onInput={(e) => setPassword(e.target.value)}
                   placeholder={t('login.password_ph')} required autocomplete="current-password" />
          </div>
          <button type="submit" class="btn btn-primary" style="width:100%" disabled={busy}>
            {busy ? t('login.signing_in') : t('login.submit')}
          </button>
        </form>
      </div>
    </section>
  );
}

export function Setup({ onSetupDone }) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  async function submit(e) {
    e.preventDefault();
    if (!username || !password || !confirm) { setError(t('setup.error_empty')); return; }
    if (password !== confirm) { setError(t('setup.error_mismatch')); return; }
    if (password.length < 8) { setError(t('setup.error_length')); return; }
    setBusy(true); setError('');
    const res = await api.post('/api/auth/setup', { username, password });
    setBusy(false);
    if (res.ok) {
      showToast(t('setup.created'), 'success');
      onSetupDone();
    } else {
      setError(t('setup.error_failed'));
    }
  }

  return (
    <section class="page auth-page">
      <div class="auth-card">
        <div class="brand-lg">mibee-rec</div>
        <h1>{t('setup.title')}</h1>
        <p class="text-muted">{t('setup.subtitle')}</p>
        {error && <div class="alert alert-error">{error}</div>}
        <form onSubmit={submit}>
          <div class="form-group">
            <label>{t('setup.username')}</label>
            <input type="text" value={username} onInput={(e) => setUsername(e.target.value)}
                   placeholder={t('setup.username_ph')} required autocomplete="username" />
          </div>
          <div class="form-group">
            <label>{t('setup.password')}</label>
            <input type="password" value={password} onInput={(e) => setPassword(e.target.value)}
                   placeholder={t('setup.password_ph')} required autocomplete="new-password" />
          </div>
          <div class="form-group">
            <label>{t('setup.confirm')}</label>
            <input type="password" value={confirm} onInput={(e) => setConfirm(e.target.value)}
                   placeholder={t('setup.confirm_ph')} required autocomplete="new-password" />
          </div>
          <button type="submit" class="btn btn-primary" style="width:100%" disabled={busy}>
            {busy ? t('setup.setting_up') : t('setup.submit')}
          </button>
        </form>
      </div>
    </section>
  );
}
