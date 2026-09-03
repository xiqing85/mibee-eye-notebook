// App bootstrap: auth state machine → capability discovery → view wiring.

import { api, onSessionExpired, refreshCapabilities } from './api.js';
import { store, setLang } from './store.js';
import { $, toast, confirmDlg } from './ui.js';
import { t, applyLang } from './i18n.js';
import { icon, initIcons } from './icons.js';
import { initTheme } from './theme.js';
import { AuthState, detectAuthState, setAuthMode, initAuth, handleLogout } from './auth.js';
import { connectEvents, disconnectEvents } from './sse.js';
import { initLive, startLive, stopLive, refreshCameraSelect, renderDetections } from './live.js';
import { initPtz, fetchPtz, updatePtzVisibility, handlePtzEvent } from './ptz.js';
import { initImaging, handleParamChanged } from './imaging.js';
import { refreshCameras, renderCameras, initCameras, stopCameras, announceRecording } from './cameras.js';
import { loadConfig, initSettings } from './settings.js';
import { checkApi, refreshStatus, initStatus, startStatusPolling } from './status.js';
import { renderDevices, initDevices } from './devices.js';

export function showView(name) {
  if (name === 'login') { teardownApp(); return; }
  document.querySelectorAll('.view').forEach((v) => v.classList.remove('active'));
  const view = $('view-' + name);
  if (view) view.classList.add('active');
  document.querySelectorAll('.nav-tab').forEach((tab) => {
    tab.classList.toggle('active', tab.dataset.view === name);
  });
  $('app').classList.remove('hidden');

  if (name === 'preview') startLive();
  else stopLive();
  if (name === 'cameras') { refreshCameras().then(renderCameras); }
  else stopCameras();
  if (name === 'settings') loadConfig();
  if (name === 'status') { checkApi(); refreshStatus(); }
  if (name === 'devices') renderDevices();
}

function teardownApp() {
  stopLive();
  stopCameras();
  disconnectEvents();
  document.querySelectorAll('.view').forEach((v) => v.classList.remove('active'));
  $('view-login').classList.add('active');
  $('app').classList.add('hidden');
}

async function enterApp() {
  await refreshCapabilities();
  await refreshCameras();
  applyLang();
  initNav();
  initLive();
  initPtz();
  initImaging();
  initCameras();
  initSettings();
  initStatus();
  initDevices();
  refreshCameraSelect();
  if (store.ptzEnabled) fetchPtz();

  connectEvents({
    ai_detection: (p) => renderDetections(p.detections || []),
    param_changed: handleParamChanged,
    recording: (p) => {
      if (p) announceRecording(!!p.active, 'info');
    },
    ptz_status: handlePtzEvent,
    camera_added: () => { refreshCameras().then(() => { renderCameras(); refreshCameraSelect(); }); },
    camera_offlined: () => { refreshCameras().then(() => { renderCameras(); refreshCameraSelect(); }); },
    status: () => { /* status view polls on its own */ },
  });

  startStatusPolling();
  showView('preview');
}

function initNav() {
  if (initNav._done) return;
  initNav._done = true;
  document.querySelectorAll('.nav-tab').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const target = btn.dataset.view;
      if (!target || target === 'login') return;
      if (store.configDirty && target !== 'settings') {
        const ok = await confirmDlg({
          message: t('unsavedConfirm'),
          okText: t('confirm'),
          cancelText: t('cancel'),
        });
        if (!ok) return;
        store.configDirty = false;
      }
      showView(target);
    });
  });
}

function initShell() {
  initIcons();
  applyLang();
  initTheme();

  document.querySelectorAll('.lang-btn').forEach((b) => {
    b.addEventListener('click', () => {
      setLang(store.lang === 'zh' ? 'en' : 'zh');
      applyLang();
    });
  });

  document.addEventListener('click', (e) => {
    const btn = e.target.closest('.password-toggle');
    if (!btn) return;
    const input = document.getElementById(btn.dataset.target);
    if (!input) return;
    const show = input.type === 'password';
    input.type = show ? 'text' : 'password';
    btn.innerHTML = icon(show ? 'eye-off' : 'eye', 18);
    btn.classList.toggle('revealed', show);
    btn.setAttribute('aria-label', show ? t('hidePassword') : t('showPassword'));
  });

  onSessionExpired(() => {
    toast(t('error'), 'error');
    showView('login');
    setAuthMode(AuthState.LOGIN);
  });
}

// Surface any uncaught error on the page — invaluable on headless devices.
window.addEventListener('error', (e) => {
  const box = document.createElement('div');
  box.style.cssText = 'position:fixed;bottom:44px;left:12px;z-index:9999;max-width:80%;'
    + 'padding:8px 12px;border-radius:8px;background:rgba(255,80,80,.92);color:#fff;'
    + 'font:12px/1.5 monospace;white-space:pre-wrap;';
  box.textContent = 'JS error: ' + (e.message || e.type) + (e.filename ? ' @ ' + e.filename + ':' + e.lineno : '');
  document.body.appendChild(box);
});
window.addEventListener('unhandledrejection', (e) => {
  const box = document.createElement('div');
  box.style.cssText = 'position:fixed;bottom:44px;left:12px;z-index:9999;max-width:80%;'
    + 'padding:8px 12px;border-radius:8px;background:rgba(255,80,80,.92);color:#fff;'
    + 'font:12px/1.5 monospace;white-space:pre-wrap;';
  box.textContent = 'Unhandled rejection: ' + (e.reason && (e.reason.stack || e.reason.message) || e.reason);
  document.body.appendChild(box);
});

document.addEventListener('DOMContentLoaded', async () => {
  initShell();
  initAuth(enterApp);
  document.querySelectorAll('.logout-btn').forEach((b) => {
    b.addEventListener('click', () => {
      // Drop tile streams BEFORE the logout POST — each hidden MJPEG <img>
      // pins one of the browser's 6 per-origin connections, and enough of
      // them starve the POST into never landing (logout looks dead).
      stopCameras();
      handleLogout(async () => {
        setAuthMode(AuthState.LOGIN);
        showView('login');
      });
    });
  });

  const state = await detectAuthState();
  if (state === AuthState.SIGNED_IN) {
    await enterApp();
  } else {
    setAuthMode(state);
    $('view-login').classList.add('active');
  }
});
