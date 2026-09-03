// Deliberate device restarts (Go config save / imaging flips / §5.1
// button) SIGTERM the service. Sessions may survive (Go persists them —
// SPEC 附录A #10) but every in-flight request breaks meanwhile; while a
// restart is pending a 401 must NOT be read as session expiry (api.js
// consults restartPending). Flow: announce → wait for the public
// /api/health → reload once, landing signed-in wherever the dialect kept
// the session.

import { toast } from './ui.js';
import { t } from './i18n.js';

let pending = false;

export function restartPending() {
  return pending;
}

export function beginDeviceRestart() {
  if (pending) return;
  pending = true;
  toast(t('restarting'), 'info');
  waitForRecovery().then((up) => {
    if (!up) toast(t('restartTimeout'), 'error');
    // Reload regardless: signed-in if the dialect kept the session,
    // login form otherwise — either landing beats a mid-flight 401.
    setTimeout(() => window.location.reload(), up ? 500 : 3000);
  });
}

async function waitForRecovery() {
  const deadline = Date.now() + 90000;
  await new Promise((r) => setTimeout(r, 3000)); // let the old process die
  for (;;) {
    try {
      // Any HTTP answer means the listener is back (health is public).
      await fetch('/api/health', { credentials: 'same-origin' });
      return true;
    } catch (_) { /* still down */ }
    if (Date.now() > deadline) return false;
    await new Promise((r) => setTimeout(r, 2000));
  }
}
