/**
 * notebook-cam — SPA Application Logic
 *
 * Vanilla JS single-page application for camera surveillance management.
 * Uses hash-based routing and fetch API with cookie-based session auth.
 *
 * This file is embedded inline in index.html for production.
 * It's kept as a separate file for development and reference.
 */

(function() {
'use strict';

// ============================================================
// STATE
// ============================================================
const state = {
  authenticated: false,
  cameras: [],
  settings: {},
  currentCamera: null,
  deleteTargetId: null,
};

// ============================================================
// API HELPER
// ============================================================
async function api(method, path, body) {
  const opts = {
    method,
    headers: { 'Accept': 'application/json' },
    credentials: 'same-origin',
  };
  if (body !== undefined) {
    opts.headers['Content-Type'] = 'application/json';
    opts.body = JSON.stringify(body);
  }
  const res = await fetch(path, opts);
  const contentType = res.headers.get('content-type') || '';
  let data = null;
  if (contentType.includes('application/json')) {
    data = await res.json();
  }
  return { ok: res.ok, status: res.status, data };
}

// ============================================================
// TOAST NOTIFICATIONS
// ============================================================
function showToast(message, type) {
  type = type || 'info';
  const container = document.getElementById('toast-container');
  const el = document.createElement('div');
  el.className = 'toast toast-' + type;
  el.textContent = message;
  container.appendChild(el);
  setTimeout(() => {
    el.style.opacity = '0';
    el.style.transition = 'opacity 300ms ease';
    setTimeout(() => el.remove(), 300);
  }, 4000);
}

// ============================================================
// NAVIGATION
// ============================================================
function navigate(hash) {
  window.location.hash = hash;
}

function getCurrentRoute() {
  const hash = window.location.hash || '#dashboard';
  const cameraMatch = hash.match(/^#\/camera\/(.+)/);
  if (cameraMatch) {
    return { page: 'camera', id: decodeURIComponent(cameraMatch[1]) };
  }
  const page = hash.replace(/^#\/?/, '') || 'dashboard';
  return { page, id: null };
}

// ============================================================
// PAGE SHOW/HIDE
// ============================================================
function showPage(pageId) {
  document.querySelectorAll('.page').forEach(p => p.classList.remove('active'));
  const page = document.getElementById('page-' + pageId);
  if (page) page.classList.add('active');
}

// ============================================================
// NAVBAR
// ============================================================
function showNavbar(show) {
  const nav = document.getElementById('navbar');
  nav.classList.toggle('hidden', !show);
}

function updateActiveNavLink(page) {
  document.querySelectorAll('.nav-link').forEach(a => {
    a.classList.toggle('active', a.dataset.page === page);
  });
}

// ============================================================
// AUTH CHECK
// ============================================================
async function checkAuth() {
  const res = await api('GET', '/api/cameras');
  if (res.status === 200) {
    state.authenticated = true;
    return { authenticated: true, setup: false };
  }
  if (res.status === 503) {
    return { authenticated: false, setup: true };
  }
  if (res.status === 401) {
    return { authenticated: false, setup: false };
  }
  return { authenticated: false, setup: false };
}

// ============================================================
// LOGIN
// ============================================================
async function handleLogin(e) {
  e.preventDefault();
  const username = document.getElementById('login-username').value.trim();
  const password = document.getElementById('login-password').value;
  const errorEl = document.getElementById('login-error');
  const submitBtn = document.getElementById('login-submit');

  if (!username || !password) {
    errorEl.textContent = 'Please enter both username and password.';
    errorEl.classList.add('show');
    return;
  }

  errorEl.classList.remove('show');
  submitBtn.disabled = true;
  submitBtn.textContent = 'Signing in...';

  try {
    const res = await api('POST', '/api/auth/login', { username, password });
    if (res.ok) {
      showToast('Signed in successfully', 'success');
      await loadDashboard();
    } else {
      errorEl.textContent = res.data && res.data.error ? res.data.error : 'Login failed';
      errorEl.classList.add('show');
    }
  } catch (_err) {
    errorEl.textContent = 'Connection error. Is the server running?';
    errorEl.classList.add('show');
  } finally {
    submitBtn.disabled = false;
    submitBtn.textContent = 'Sign In';
  }
}

// ============================================================
// SETUP (first-run)
// ============================================================
async function handleSetup(e) {
  e.preventDefault();
  const username = document.getElementById('setup-username').value.trim();
  const password = document.getElementById('setup-password').value;
  const confirm = document.getElementById('setup-confirm').value;
  const errorEl = document.getElementById('setup-error');
  const submitBtn = document.getElementById('setup-submit');

  if (!username || !password) {
    errorEl.textContent = 'Please fill in all fields.';
    errorEl.classList.add('show');
    return;
  }

  if (password !== confirm) {
    errorEl.textContent = 'Passwords do not match.';
    errorEl.classList.add('show');
    return;
  }

  if (password.length < 6) {
    errorEl.textContent = 'Password must be at least 6 characters.';
    errorEl.classList.add('show');
    return;
  }

  errorEl.classList.remove('show');
  submitBtn.disabled = true;
  submitBtn.textContent = 'Setting up...';

  try {
    const res = await api('POST', '/api/auth/setup', { username, password });
    if (res.ok) {
      showToast('Account created! Please sign in.', 'success');
      showPage('login');
      document.getElementById('login-username').value = username;
      document.getElementById('login-password').value = '';
    } else {
      errorEl.textContent = res.data && res.data.error ? res.data.error : 'Setup failed';
      errorEl.classList.add('show');
    }
  } catch (_err) {
    errorEl.textContent = 'Connection error. Is the server running?';
    errorEl.classList.add('show');
  } finally {
    submitBtn.disabled = false;
    submitBtn.textContent = 'Create Account';
  }
}

// ============================================================
// LOGOUT
// ============================================================
async function handleLogout() {
  try {
    await api('POST', '/api/auth/logout');
  } catch (_) { /* ignore */ }
  state.authenticated = false;
  state.cameras = [];
  showNavbar(false);
  showPage('login');
  window.location.hash = '#login';
  showToast('Signed out', 'info');
}

// ============================================================
// DASHBOARD — CAMERA LIST
// ============================================================
async function loadDashboard() {
  showPage('dashboard');
  showNavbar(true);
  updateActiveNavLink('dashboard');

  document.getElementById('camera-loading').classList.remove('hidden');
  document.getElementById('camera-empty').classList.add('hidden');
  document.getElementById('camera-table-container').classList.add('hidden');

  try {
    const res = await api('GET', '/api/cameras');
    if (!res.ok) {
      if (res.status === 401) {
        state.authenticated = false;
        showNavbar(false);
        showPage('login');
        return;
      }
      showToast('Failed to load cameras: ' + (res.data && res.data.error || 'Unknown error'), 'error');
      document.getElementById('camera-loading').classList.add('hidden');
      return;
    }
    state.cameras = Array.isArray(res.data) ? res.data : [];
    renderCameraTable();
  } catch (_err) {
    showToast('Connection error loading cameras', 'error');
    document.getElementById('camera-loading').classList.add('hidden');
  }
}

function renderCameraTable() {
  document.getElementById('camera-loading').classList.add('hidden');

  if (state.cameras.length === 0) {
    document.getElementById('camera-empty').classList.remove('hidden');
    document.getElementById('camera-table-container').classList.add('hidden');
    return;
  }

  document.getElementById('camera-empty').classList.add('hidden');
  document.getElementById('camera-table-container').classList.remove('hidden');

  const tbody = document.getElementById('camera-table-body');
  tbody.innerHTML = '';

  state.cameras.forEach(cam => {
    const tr = document.createElement('tr');
    const badgeClass = 'badge-' + (cam.status === 'running' ? 'streaming' : cam.status === 'stopped' ? 'stopped' : 'error');

    tr.innerHTML =
      '<td><a href="#/camera/' + encodeURIComponent(cam.id) + '" style="font-weight:500">' + html(cam.name) + '</a></td>' +
      '<td><span class="text-muted" style="font-family:var(--font-mono);font-size:0.857rem">' + html(formatType(cam.camera_type)) + '</span></td>' +
      '<td><span class="badge ' + badgeClass + '">' + html(cam.status) + '</span></td>' +
      '<td class="cell-actions">' +
        streamActionsHtml(cam) +
        '<button class="btn btn-sm btn-secondary" onclick="window.navigate(\'#/camera/' + encodeURIComponent(cam.id) + '\')">View</button>' +
        '<button class="btn btn-sm btn-danger" onclick="window.showDeleteConfirm(\'' + encodeURIComponent(cam.id) + '\', \'' + htmlAttr(cam.name) + '\')">Delete</button>' +
      '</td>';

    tbody.appendChild(tr);
  });
}

function formatType(type) {
  return ({ usb: 'USB', rtsp: 'RTSP', onvif: 'ONVIF', gb28181: 'GB/T 28181', rtmp: 'RTMP' })[type] || type;
}

function streamActionsHtml(cam) {
  if (cam.status === 'running') {
    return '<button class="btn btn-sm btn-danger" onclick="window.stopStream(\'' + encodeURIComponent(cam.id) + '\')">Stop</button>';
  }
  return '<button class="btn btn-sm btn-primary" onclick="window.startStream(\'' + encodeURIComponent(cam.id) + '\')">Start</button>';
}

// ============================================================
// STREAM CONTROL
// ============================================================
async function startStream(id) {
  try {
    const res = await api('POST', '/api/cameras/' + id + '/start');
    if (res.ok) {
      showToast('Stream started', 'success');
      await loadDashboard();
    } else {
      showToast(res.data && res.data.error ? res.data.error : 'Failed to start stream', 'error');
    }
  } catch (_) {
    showToast('Connection error', 'error');
  }
}

async function stopStream(id) {
  try {
    const res = await api('POST', '/api/cameras/' + id + '/stop');
    if (res.ok) {
      showToast('Stream stopped', 'success');
      await loadDashboard();
    } else {
      showToast(res.data && res.data.error ? res.data.error : 'Failed to stop stream', 'error');
    }
  } catch (_) {
    showToast('Connection error', 'error');
  }
}

// ============================================================
// ADD CAMERA MODAL
// ============================================================
function showAddCameraModal() {
  document.getElementById('modal-title').textContent = 'Add Camera';
  document.getElementById('cam-name').value = '';
  document.getElementById('cam-type').value = '';
  document.getElementById('cam-config').value = '';
  document.getElementById('modal-error').classList.remove('show');
  document.getElementById('modal-error').textContent = '';
  document.getElementById('modal-save-btn').textContent = 'Add Camera';
  document.getElementById('modal-overlay').classList.remove('hidden');
  document.getElementById('cam-name').focus();
}

function closeModal() {
  document.getElementById('modal-overlay').classList.add('hidden');
}

async function saveCamera() {
  const name = document.getElementById('cam-name').value.trim();
  const type = document.getElementById('cam-type').value;
  const configStr = document.getElementById('cam-config').value.trim();
  const errorEl = document.getElementById('modal-error');
  const saveBtn = document.getElementById('modal-save-btn');

  if (!name) {
    errorEl.textContent = 'Camera name is required.';
    errorEl.classList.add('show');
    return;
  }
  if (!type) {
    errorEl.textContent = 'Camera type is required.';
    errorEl.classList.add('show');
    return;
  }

  let config = {};
  if (configStr) {
    try {
      config = JSON.parse(configStr);
    } catch (_) {
      errorEl.textContent = 'Invalid JSON in configuration field.';
      errorEl.classList.add('show');
      return;
    }
  }

  errorEl.classList.remove('show');
  saveBtn.disabled = true;
  saveBtn.textContent = 'Saving...';

  try {
    const res = await api('POST', '/api/cameras', { name, camera_type: type, config });
    if (res.ok) {
      showToast('Camera added', 'success');
      closeModal();
      await loadDashboard();
    } else {
      errorEl.textContent = res.data && res.data.error ? res.data.error : 'Failed to add camera';
      errorEl.classList.add('show');
    }
  } catch (_) {
    errorEl.textContent = 'Connection error';
    errorEl.classList.add('show');
  } finally {
    saveBtn.disabled = false;
    saveBtn.textContent = 'Add Camera';
  }
}

// ============================================================
// DELETE CONFIRM
// ============================================================
function showDeleteConfirm(id, name) {
  state.deleteTargetId = decodeURIComponent(id);
  document.getElementById('confirm-text').textContent =
    'Are you sure you want to delete "' + name + '"? This action cannot be undone.';
  document.getElementById('confirm-overlay').classList.remove('hidden');
}

function closeConfirm() {
  document.getElementById('confirm-overlay').classList.add('hidden');
  state.deleteTargetId = null;
}

async function confirmDelete() {
  const id = state.deleteTargetId;
  if (!id) return;

  const btn = document.getElementById('confirm-delete-btn');
  btn.disabled = true;
  btn.textContent = 'Deleting...';

  try {
    const res = await api('DELETE', '/api/cameras/' + id);
    if (res.ok) {
      showToast('Camera deleted', 'success');
      closeConfirm();
      await loadDashboard();
    } else {
      showToast(res.data && res.data.error ? res.data.error : 'Failed to delete camera', 'error');
      closeConfirm();
    }
  } catch (_) {
    showToast('Connection error', 'error');
    closeConfirm();
  } finally {
    btn.disabled = false;
    btn.textContent = 'Delete';
  }
}

// ============================================================
// CAMERA LIVE VIEW
// ============================================================
async function loadCameraView(id) {
  showPage('camera');
  showNavbar(true);
  updateActiveNavLink('');

  document.getElementById('camera-view-name').textContent = 'Loading...';
  document.getElementById('camera-view-status').textContent = '';

  try {
    const res = await api('GET', '/api/cameras/' + id);
    if (!res.ok) {
      showToast('Failed to load camera', 'error');
      navigate('#dashboard');
      return;
    }
    const cam = res.data;
    state.currentCamera = cam;

    document.getElementById('camera-view-name').textContent = cam.name;
    document.getElementById('camera-view-status').textContent = cam.status;

    document.getElementById('cam-info-id').textContent = cam.id;
    document.getElementById('cam-info-type').textContent = formatType(cam.camera_type);
    document.getElementById('cam-info-status').textContent = cam.status;
    document.getElementById('cam-info-created').textContent = cam.created_at || '-';

    let rtspUrl = '';
    if (cam.config && cam.config.url) {
      rtspUrl = cam.config.url;
    } else {
      rtspUrl = 'rtsp://localhost:8554/' + cam.id;
    }
    document.getElementById('rtsp-url').textContent = rtspUrl;
  } catch (_err) {
    showToast('Connection error', 'error');
    navigate('#dashboard');
  }
}

function copyRtspUrl() {
  const url = document.getElementById('rtsp-url').textContent;
  if (!url) return;
  if (navigator.clipboard) {
    navigator.clipboard.writeText(url).then(() => {
      showToast('RTSP URL copied to clipboard', 'success');
    }).catch(() => fallbackCopy(url));
  } else {
    fallbackCopy(url);
  }
}

function fallbackCopy(text) {
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  document.execCommand('copy');
  ta.remove();
  showToast('RTSP URL copied', 'success');
}

// ============================================================
// SETTINGS
// ============================================================
async function loadSettings() {
  showPage('settings');
  showNavbar(true);
  updateActiveNavLink('settings');

  document.getElementById('settings-loading').classList.remove('hidden');
  document.getElementById('settings-form').classList.add('hidden');

  try {
    const res = await api('GET', '/api/settings');
    if (!res.ok) {
      if (res.status === 401) {
        state.authenticated = false;
        showNavbar(false);
        showPage('login');
        return;
      }
      showToast('Failed to load settings', 'error');
      document.getElementById('settings-loading').classList.add('hidden');
      return;
    }
    state.settings = res.data || {};

    const fields = [
      'web_port', 'rtsp_port', 'mibee_url', 'mibee_api_key',
      'recordings_path', 'max_segment_duration',
    ];
    fields.forEach(key => {
      const el = document.getElementById('setting-' + key);
      if (el) {
        el.value = state.settings[key] || '';
      }
    });

    document.getElementById('settings-loading').classList.add('hidden');
    document.getElementById('settings-form').classList.remove('hidden');
  } catch (_err) {
    showToast('Connection error loading settings', 'error');
    document.getElementById('settings-loading').classList.add('hidden');
  }
}

async function saveSettings() {
  const settings = {};
  const fields = [
    'web_port', 'rtsp_port', 'mibee_url', 'mibee_api_key',
    'recordings_path', 'max_segment_duration',
  ];
  fields.forEach(key => {
    const el = document.getElementById('setting-' + key);
    if (el) {
      const val = el.value.trim();
      if (val) settings[key] = val;
    }
  });

  const saveBtn = document.getElementById('settings-save-btn');
  saveBtn.disabled = true;
  saveBtn.textContent = 'Saving...';

  try {
    const res = await api('PUT', '/api/settings', { settings });
    if (res.ok) {
      showToast('Settings saved', 'success');
    } else {
      showToast(res.data && res.data.error ? res.data.error : 'Failed to save settings', 'error');
    }
  } catch (_) {
    showToast('Connection error saving settings', 'error');
  } finally {
    saveBtn.disabled = false;
    saveBtn.textContent = 'Save Changes';
  }
}

// ============================================================
// ROUTER
// ============================================================
async function route() {
  const route = getCurrentRoute();

  switch (route.page) {
    case 'login':
      showNavbar(false);
      showPage('login');
      break;
    case 'setup':
      showNavbar(false);
      showPage('setup');
      break;
    case 'camera':
      if (!state.authenticated) { await initApp(); return; }
      await loadCameraView(route.id);
      break;
    case 'settings':
      if (!state.authenticated) { await initApp(); return; }
      await loadSettings();
      break;
    case 'dashboard':
    default:
      if (!state.authenticated) { await initApp(); return; }
      await loadDashboard();
      break;
  }
}

// ============================================================
// INIT
// ============================================================
async function initApp() {
  const status = await checkAuth();

  if (status.authenticated) {
    state.authenticated = true;
    showNavbar(true);
    await loadDashboard();
  } else if (status.setup) {
    state.authenticated = false;
    showNavbar(false);
    showPage('setup');
    window.location.hash = '#setup';
  } else {
    state.authenticated = false;
    showNavbar(false);
    showPage('login');
    window.location.hash = '#login';
  }
}

// ============================================================
// UTILITY
// ============================================================
function html(str) {
  if (typeof str !== 'string') return '';
  return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function htmlAttr(str) {
  if (typeof str !== 'string') return '';
  return str.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}

// ============================================================
// EVENT BINDING
// ============================================================
document.addEventListener('DOMContentLoaded', function() {
  document.getElementById('login-form').addEventListener('submit', handleLogin);
  document.getElementById('setup-form').addEventListener('submit', handleSetup);
  document.getElementById('logout-btn').addEventListener('click', handleLogout);

  document.getElementById('modal-overlay').addEventListener('click', function(e) {
    if (e.target === this) closeModal();
  });
  document.getElementById('confirm-overlay').addEventListener('click', function(e) {
    if (e.target === this) closeConfirm();
  });

  document.addEventListener('keydown', function(e) {
    if (e.key === 'Escape') {
      if (!document.getElementById('modal-overlay').classList.contains('hidden')) closeModal();
      if (!document.getElementById('confirm-overlay').classList.contains('hidden')) closeConfirm();
    }
  });

  window.addEventListener('hashchange', route);
  initApp();
});

// Expose functions for onclick handlers
window.navigate = navigate;
window.showAddCameraModal = showAddCameraModal;
window.closeModal = closeModal;
window.saveCamera = saveCamera;
window.showDeleteConfirm = showDeleteConfirm;
window.closeConfirm = closeConfirm;
window.confirmDelete = confirmDelete;
window.startStream = startStream;
window.stopStream = stopStream;
window.copyRtspUrl = copyRtspUrl;
window.saveSettings = saveSettings;

})();
