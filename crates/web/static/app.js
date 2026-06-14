/**
 * MiBee-Rec — SPA Application Logic (reference source)
 *
 * NOTE: This file is the CANONICAL SOURCE for the SPA JavaScript.
 * The production index.html has this code inlined (and minified).
 * When making changes, update BOTH this file AND index.html.
 *
 * Naming: abbreviated function/property names match index.html.
 * Mapping table:
 *   S   = state           A   = api              T   = showToast
 *   G   = navigate        R   = getCurrentRoute  P   = showPage
 *   N   = showNavbar      U   = updateActiveNav   C   = checkAuth
 *   L   = handleLogin     H   = handleSetup      O   = handleLogout
 *   D   = loadDashboard   V   = renderCameraTable F   = formatType
 *   Y   = startStream     w   = stopStream        wa  = showAddCameraModal
 *   cm  = closeModal      sc  = saveCamera        x   = showDeleteConfirm
 *   cC  = closeConfirm    cD  = confirmDelete     K   = loadCameraView
 *   cr  = copyRtspUrl     fC  = fallbackCopy      r   = route
 *   i   = initApp         E   = html escape       J   = htmlAttr escape
 *   q   = toast queue     M   = loadSettings      Q   = saveSettings
 *
 * State properties:
 *   S.a = authenticated    S.c = cameras           S.s = settings
 *   S.dc = deleteTargetId  S.urls = stream RTSP URLs
 */

(function() {
'use strict';

// ============================================================
// STATE
// ============================================================
let S = { a: 0, c: [], s: {}, dc: null, urls: {}, lf: null };
// Toast queue for FIFO capping (F12)
let q = [];

// ============================================================
// API HELPER
// ============================================================
async function A(m, p, b) {
  let o = {
    method: m,
    headers: { 'Accept': 'application/json' },
    credentials: 'same-origin',
  };
  if (b !== undefined) {
    o.headers['Content-Type'] = 'application/json';
    o.body = JSON.stringify(b);
  }
  let r = await fetch(p, o);

  // Global 401 handler (F10)
  if (r.status === 401 && S.a) {
    S.a = 0;
    T('Session expired', 'i');
    setTimeout(() => { N(0); P('login'); window.location.hash = '#login'; }, 1500);
  }

  let d = null;
  if ((r.headers.get('content-type') || '').includes('application/json')) {
    d = await r.json();
  }
  return { ok: r.ok, status: r.status, data: d };
}

// ============================================================
// TOAST (F12: cap at 3, FIFO)
// ============================================================
function T(msg, t) {
  t = t || 'info';

  // Cap at 3, dismiss oldest (F12)
  while (q.length >= 3) {
    let o = q.shift();
    if (o && o.parentNode) o.remove();
  }

  let e = document.createElement('div');
  e.className = 'to-' + t;
  e.textContent = msg;
  document.getElementById('to').appendChild(e);
  q.push(e);

  setTimeout(() => {
    if (e.parentNode) {
      e.style.opacity = '0';
      e.style.transition = 'opacity 300ms ease';
      setTimeout(() => e.remove(), 300);
    }
    let i = q.indexOf(e);
    if (i !== -1) q.splice(i, 1);
  }, 4000);
}

// ============================================================
// NAVIGATION
// ============================================================
function G(h) { window.location.hash = h; }

function R() {
  let h = window.location.hash || '#db';
  let m = h.match(/^#\/c\/(.+)/);
  if (m) return { p: 'c', id: decodeURIComponent(m[1]) };
  let pg = h.replace(/^#\/?/, '') || 'db';
  return { p: pg, id: null };
}

// ============================================================
// PAGE SHOW/HIDE
// ============================================================
function P(id) {
  document.querySelectorAll('.pg').forEach(p => {
    p.classList.remove('a');
    p.classList.remove('ap');
  });
  let e = document.getElementById('pg-' + id);
  if (e) {
    e.classList.add('a');
    if (id === 'login' || id === 'stp') e.classList.add('ap');
  }
}

// ============================================================
// NAVBAR
// ============================================================
function N(s) { document.getElementById('nv').classList.toggle('h', !s); }

function U(pg) {
  document.querySelectorAll('.nl a').forEach(a =>
    a.classList.toggle('active', a.dataset.p === pg)
  );
}

// ============================================================
// AUTH CHECK
// ============================================================
async function C() {
  let r = await A('GET', '/api/cameras');
  if (r.status === 200)  { S.a = 1; return { a: 1, s: 0 }; }
  if (r.status === 503)  return { a: 0, s: 1 };
  return { a: 0, s: 0 };
}

// ============================================================
// LOGIN
// ============================================================
async function L(e) {
  e.preventDefault();
  let u = document.getElementById('lu').value.trim();
  let p = document.getElementById('lp').value;
  let el = document.getElementById('le');
  let sb = document.getElementById('ls');

  if (!u || !p) {
    el.textContent = 'Please enter both username and password.';
    el.classList.add('s');
    return;
  }

  el.classList.remove('s');
  sb.disabled = 1;
  sb.textContent = 'Signing in...';

  try {
    let r = await A('POST', '/api/auth/login', { username: u, password: p });
    if (r.ok) {
      T('Signed in', 'success');
      await D();
    } else {
      el.textContent = r.data && r.data.error ? r.data.error : 'Login failed';
      el.classList.add('s');
    }
  } catch (_) {
    el.textContent = 'Cannot reach server. Check it\'s running and reload.';
    el.classList.add('s');
  } finally {
    sb.disabled = 0;
    sb.textContent = 'Sign In';
  }
}

// ============================================================
// SETUP (first-run)
// ============================================================
async function H(e) {
  e.preventDefault();
  let u = document.getElementById('su').value.trim();
  let p = document.getElementById('sp').value;
  let c = document.getElementById('sc').value;
  let el = document.getElementById('se');
  let sb = document.getElementById('ss');

  if (!u || !p) {
    el.textContent = 'Please fill all fields.';
    el.classList.add('s');
    return;
  }

  if (p !== c) {
    el.textContent = 'Passwords do not match.';
    el.classList.add('s');
    return;
  }

  // B1: backend requires >= 8
  if (p.length < 8) {
    el.textContent = 'Password must be at least 8 characters.';
    el.classList.add('s');
    return;
  }

  el.classList.remove('s');
  sb.disabled = 1;
  sb.textContent = 'Setting up...';

  try {
    let r = await A('POST', '/api/auth/setup', { username: u, password: p });
    if (r.ok) {
      T('Account created! Sign in.', 'success');
      P('login');
      document.getElementById('lu').value = u;
      document.getElementById('lp').value = '';
    } else {
      el.textContent = r.data && r.data.error ? r.data.error : 'Setup failed';
      el.classList.add('s');
    }
  } catch (_) {
    el.textContent = 'Cannot reach server. Check it\'s running and reload.';
    el.classList.add('s');
  } finally {
    sb.disabled = 0;
    sb.textContent = 'Create Account';
  }
}

// ============================================================
// LOGOUT
// ============================================================
async function O() {
  try { await A('POST', '/api/auth/logout'); } catch (_) {}
  S.a = 0;
  S.c = [];
  N(0);
  P('login');
  window.location.hash = '#login';
  T('Signed out', 'info');
}

// ============================================================
// DASHBOARD — CAMERA LIST
// ============================================================
async function D() {
  P('db');
  N(1);
  U('db');

  document.getElementById('cl').classList.remove('h');
  document.getElementById('ce').classList.add('h');
  document.getElementById('ct').classList.add('h');

  try {
    let r = await A('GET', '/api/cameras');
    if (!r.ok) {
      // 401 now handled globally in A() (F10)
      T('Failed: ' + (r.data && r.data.error || 'Unknown'), 'e');
      document.getElementById('cl').classList.add('h');
      return;
    }
    S.c = Array.isArray(r.data) ? r.data : [];
    V();
    spoll();
  } catch (_) {
    T('Cannot reach server. Check it\'s running and reload.', 'e');
    document.getElementById('cl').classList.add('h');
  }
}

function V() {
  document.getElementById('cl').classList.add('h');

  if (S.c.length === 0) {
    document.getElementById('ce').classList.remove('h');
    document.getElementById('ct').classList.add('h');
    return;
  }

  document.getElementById('ce').classList.add('h');
  document.getElementById('ct').classList.remove('h');

  let b = document.getElementById('cb');
  b.innerHTML = '';

  S.c.forEach(c => {
    let tr = document.createElement('tr');
    let bc = 'bg-' + (
      c.status === 'running' ? 'r' :
      c.status === 'stopped'  ? 'p' :
      'e'
    );
    let id = encodeURIComponent(c.id);

    tr.innerHTML =
      '<td data-label="Name"><a href="#/c/' + id + '" style=font-weight:500>' + E(c.name) + '</a></td>' +
      '<td data-label="Type"><span class=tm style="font-family:var(--mo);font-size:.857rem">' + E(F(c.camera_type)) + '</span></td>' +
      '<td data-label="Status"><span class="bg ' + bc + '">' + E(c.status) + '</span></td>' +
      '<td data-label="Stream URL">' + (
        c.status === 'running'
          ? '<div style="display:flex;align-items:center;gap:var(--s4)">' +
            '<code style="font-family:var(--mo);font-size:.786rem;color:var(--t2)">' +
            E(S.urls[c.id] || 'rtsp://localhost:8554/live/' + id) +
            '</code>' +
            '<button class="b bsm bs" data-url="' + J(S.urls[c.id] || 'rtsp://localhost:8554/live/' + id) + '" ' +
            'onclick="copyUrl(this.dataset.url)" aria-label="Copy RTSP URL to clipboard">Copy</button></div>'
          : '-'
      ) + '</td>' +
      '<td data-label="Actions" class=ca>' +
        (c.status === 'running'
          ? '<button class="b bd bsm" onclick=w("' + id + '") aria-label="Stop stream">Stop</button>'
          : '<button class="b bp bsm" onclick=Y("' + id + '") aria-label="Start stream">Start</button>'
        ) +
        '<button class="b bs bsm" onclick=G("#/c/' + id + '") aria-label="View camera details">View</button>' +
        '<button class="b bs bsm" onclick=ec(' + id + ') aria-label="Edit camera">Edit</button>' +
        '<button class="b bd bsm" onclick=x("' + id + '","' + J(c.name) + '") aria-label="Delete camera">Delete</button>' +
      '</td>';

    b.appendChild(tr);
  });
}

function F(t) {
  return ({ usb: 'USB', rtsp: 'RTSP', onvif: 'ONVIF', gb28181: 'GB/T 28181', rtmp: 'RTMP' })[t] || t;
}

// ============================================================
// STREAM CONTROL
// ============================================================
async function Y(id) {
  let r = await A('POST', '/api/cameras/' + id + '/start');
  if (r.ok) {
    if (r.data && r.data.rtsp_url) {
      S.urls[r.data.camera_id || decodeURIComponent(id)] = r.data.rtsp_url;
    }
    T('Stream started', 's');
    await D();
  } else {
    T(r.data && r.data.error ? r.data.error : 'Failed', 'e');
  }
}

async function w(id) {
  let r = await A('POST', '/api/cameras/' + id + '/stop');
  if (r.ok) {
    try { delete S.urls[decodeURIComponent(id)]; } catch (_) {}
    T('Stream stopped', 's');
    await D();
  } else {
    T(r.data && r.data.error ? r.data.error : 'Failed', 'e');
  }
}

// ============================================================
// ADD CAMERA MODAL
// ============================================================
function wa() {
  S.editId = null;
  document.getElementById('mt').textContent = 'Add Camera';
  document.getElementById('cn2').value = '';
  document.getElementById('cty').value = '';
  document.getElementById('cc').value = '';
  document.getElementById('me').classList.remove('s');
  document.getElementById('me').textContent = '';
  document.getElementById('msb').textContent = 'Add Camera';

  let dg = document.getElementById('cam-device-group');
  if (dg) dg.classList.add('h');

  let ds = document.getElementById('cam-device-select');
  if (ds) {
    ds.innerHTML = '<option value="">Select a device...</option>';
    ds.disabled = true;
  }

  S.lf = document.activeElement;
  document.getElementById('mo').classList.remove('h');
  document.getElementById('cn2').focus();
}

function cm() { document.getElementById('mo').classList.add('h'); if (S.lf) { try { S.lf.focus(); } catch (_) {} S.lf = null; } }

async function sc() {
  let n = document.getElementById('cn2').value.trim();
  let t = document.getElementById('cty').value;
  let cfg = document.getElementById('cc').value.trim();
  let el = document.getElementById('me');
  let sb = document.getElementById('msb');

  if (!n)  { el.textContent = 'Name required.'; el.classList.add('s'); return; }
  if (!t)  { el.textContent = 'Type required.'; el.classList.add('s'); return; }

  let c = {};
  if (cfg) {
    try { c = JSON.parse(cfg); }
    catch (_) { el.textContent = 'Invalid JSON.'; el.classList.add('s'); return; }
  }

  let dg = document.getElementById('cam-device-group');
  let ds = document.getElementById('cam-device-select');
  if (dg && ds && !dg.classList.contains('h') && ds.value) {
    c.device_index = parseInt(ds.value);
  }

  el.classList.remove('s');
  sb.disabled = 1;
  sb.textContent = 'Saving...';

  try {
    let r;
    if (S.editId) {
      r = await A('PUT', '/api/cameras/' + S.editId, { name: n, camera_type: t, config: c });
    } else {
      r = await A('POST', '/api/cameras', { name: n, camera_type: t, config: c });
    }
    if (r.ok) {
      T(S.editId ? 'Camera updated' : 'Camera added', 's');
      delete S.editId;
      cm();
      await D();
    } else {
      el.textContent = r.data && r.data.error ? r.data.error : 'Failed';
      el.classList.add('s');
    }
  } catch (_) {
    el.textContent = 'Cannot reach server. Check it\'s running and reload.';
    el.classList.add('s');
  } finally {
    sb.disabled = 0;
    sb.textContent = 'Add Camera';
  }
}

// ============================================================
// DELETE CONFIRM
// ============================================================
function x(id, nm) {
  S.dc = decodeURIComponent(id);
  document.getElementById('ct2').textContent = 'Delete "' + nm + '"? This cannot be undone.';
  S.lf = document.activeElement;
  document.getElementById('co').classList.remove('h');
  let fb = document.getElementById('cdb');
  if (fb) setTimeout(function() { fb.focus(); }, 50);
}

function cC() {
  document.getElementById('co').classList.add('h');
  if (S.lf) { try { S.lf.focus(); } catch (_) {} S.lf = null; }
  S.dc = null;
}

async function cD() {
  let id = S.dc;
  if (!id) return;

  let btn = document.getElementById('cdb');
  btn.disabled = 1;
  btn.textContent = 'Deleting...';

  try {
    let r = await A('DELETE', '/api/cameras/' + id);
    if (r.ok) {
      T('Deleted', 's');
      cC();
      await D();
    } else {
      T(r.data && r.data.error ? r.data.error : 'Failed', 'e');
      cC();
    }
  } catch (_) {
    T('Cannot reach server. Check it\'s running and reload.', 'e');
    cC();
  } finally {
    btn.disabled = 0;
    btn.textContent = 'Delete';
  }
}

// ============================================================
// FOCUS TRAP
// ============================================================
function ft(m, e) {
  let f = m.querySelectorAll('button,input,select,textarea,a[href],[tabindex]:not([tabindex="-1"])');
  if (f.length === 0) return;
  let fi = f[0], la = f[f.length - 1];
  if (e.shiftKey && document.activeElement === fi) { e.preventDefault(); la.focus(); }
  else if (!e.shiftKey && document.activeElement === la) { e.preventDefault(); fi.focus(); }
}

// ============================================================
// CAMERA LIVE VIEW
// ============================================================
async function K(id) {
  P('c');
  N(1);
  U('');

  document.getElementById('cn').textContent = 'Loading...';
  document.getElementById('cs').textContent = '';

  try {
    let r = await A('GET', '/api/cameras/' + id);
    if (!r.ok) {
      T('Failed to load camera', 'e');
      G('#db');
      return;
    }
    let c = r.data;

    document.getElementById('cn').textContent = c.name;
    document.getElementById('cs').textContent = c.status;
    document.getElementById('ci-').textContent = c.id;
    document.getElementById('cit').textContent = F(c.camera_type);
    document.getElementById('cis').textContent = c.status;
    document.getElementById('cic').textContent = c.created_at || '-';
    document.getElementById('ru').textContent = c.config && c.config.url
      ? c.config.url
      : 'rtsp://localhost:8554/' + c.id;
  } catch (_) {
    T('Cannot reach server. Check it\'s running and reload.', 'e');
    G('#db');
  }
}

function cr() {
  let u = document.getElementById('ru').textContent;
  if (!u) return;
  if (navigator.clipboard) {
    navigator.clipboard.writeText(u)
      .then(() => T('RTSP URL copied', 's'))
      .catch(() => fC(u));
  } else {
    fC(u);
  }
}

function fC(t) {
  let ta = document.createElement('textarea');
  ta.value = t;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  document.execCommand('copy');
  ta.remove();
  T('RTSP URL copied', 's');
}

function copyUrl(u) {
  if (navigator.clipboard) {
    navigator.clipboard.writeText(u)
      .then(() => T('URL copied', 's'))
      .catch(() => fC(u));
  } else {
    fC(u);
  }
}

// ============================================================
// DEVICE SELECTOR (USB device for camera creation)
// ============================================================
async function loadDeviceSelect() {
  let ds = document.getElementById('cam-device-select');
  if (!ds) return;
  ds.disabled = true;
  ds.innerHTML = '<option value="">Loading...</option>';
  try {
    let r = await A('GET', '/api/devices/video');
    if (r.ok && Array.isArray(r.data)) {
      ds.innerHTML = '<option value="">Select a device...</option>';
      r.data.forEach(function(d, i) {
        let o = document.createElement('option');
        o.value = d.index !== undefined ? d.index : i;
        o.textContent = d.name || ('Device ' + o.value);
        ds.appendChild(o);
      });
      ds.disabled = false;
    } else {
      ds.innerHTML = '<option value="">No devices found</option>';
    }
  } catch (_) {
    ds.innerHTML = '<option value="">Error loading devices</option>';
  }
}

function setupDeviceSelector() {
  let s = document.getElementById('cty');
  let dg = document.getElementById('cam-device-group');
  if (!s || !dg) return;
  s.addEventListener('change', function() {
    if (this.value === 'usb') {
      dg.classList.remove('h');
      loadDeviceSelect();
    } else {
      dg.classList.add('h');
    }
  });
}

// ============================================================
// SETTINGS
// ============================================================
async function M() {
  P('st');
  N(1);
  U('st');

  document.getElementById('sl').classList.remove('h');
  document.getElementById('sfm').classList.add('h');

  try {
    let r = await A('GET', '/api/settings');
    if (!r.ok) {
      // 401 now handled globally in A() (F10)
      T('Failed to load settings', 'e');
      document.getElementById('sl').classList.add('h');
      return;
    }
    S.s = r.data || {};

    let map = {
      web_port: 'swp', rtsp_port: 'srp',
      mibee_url: 'smu', mibee_api_key: 'smk',
      recordings_path: 'srp2', max_segment_duration: 'smd',
    };
    ['web_port', 'rtsp_port', 'mibee_url', 'mibee_api_key',
     'recordings_path', 'max_segment_duration'].forEach(k => {
      let el = document.getElementById(map[k]);
      if (el) el.value = S.s[k] || '';
    });

    document.getElementById('sl').classList.add('h');
    document.getElementById('sfm').classList.remove('h');
    lp();
  } catch (_) {
    T('Cannot reach server. Check it\'s running and reload.', 'e');
    document.getElementById('sl').classList.add('h');
  }
}

async function Q() {
  let s = {};

  let map = {
    web_port: 'swp', rtsp_port: 'srp',
    mibee_url: 'smu', mibee_api_key: 'smk',
    recordings_path: 'srp2', max_segment_duration: 'smd',
  };
  ['web_port', 'rtsp_port', 'mibee_url', 'mibee_api_key',
   'recordings_path', 'max_segment_duration'].forEach(k => {
    let el = document.getElementById(map[k]);
    if (el) {
      let v = el.value.trim();
      if (v) s[k] = v;
    }
  });

  // F13: disable button while in-flight
  let btn = document.getElementById('sb');
  btn.disabled = 1;
  btn.textContent = 'Saving...';

  try {
    let r = await A('PUT', '/api/settings', { settings: s });
    if (r.ok) {
      T('Settings saved', 's');
    } else {
      T(r.data && r.data.error ? r.data.error : 'Failed', 'e');
    }
  } catch (_) {
    T('Cannot reach server. Check it\'s running and reload.', 'e');
  } finally {
    btn.disabled = 0;
    btn.textContent = 'Save Changes';
  }
}

// ============================================================
// DEVICES PAGE (Video/Audio discovery)
// ============================================================
async function loadDevices() {
  try {
    let vr = await A('GET', '/api/devices/video');
    let ar = await A('GET', '/api/devices/audio');

    let vd = document.getElementById('video-devices');
    let ad = document.getElementById('audio-devices');

    // Video devices
    let vh = '<h3 style="margin-bottom:var(--s16)">Video Devices</h3>';
    if (vr.ok && Array.isArray(vr.data) && vr.data.length > 0) {
      vr.data.forEach(function(d) {
        vh += '<div class="device-card">' +
              '<div class="device-name">' + E(d.name) + '</div>' +
              '<div class="device-info">Index: ' + d.index + '</div>';
        if (d.formats && d.formats.length > 0) {
          vh += '<div class="device-formats">Formats: ' + E(d.formats.join(', ')) + '</div>';
        }
        vh += '<button class="b bp bsm" onclick="createCameraFromDevice(' + d.index + ')">Use as Camera</button>' +
              '</div>';
      });
    } else {
      vh += '<p class="tm">No video devices found.</p>';
    }
    vd.innerHTML = vh;

    // Audio devices
    let ah = '<h3 style="margin-bottom:var(--s16)">Audio Devices</h3>';
    if (ar.ok && Array.isArray(ar.data) && ar.data.length > 0) {
      ar.data.forEach(function(d) {
        ah += '<div class="device-card">' +
              '<div class="device-name">' + E(d.name) + '</div>';
        if (d.supported_configs && d.supported_configs.length > 0) {
          ah += '<div class="device-info">Configs: ' + E(d.supported_configs.join(', ')) + '</div>';
        }
        ah += '</div>';
      });
    } else {
      ah += '<p class="tm">No audio devices found.</p>';
    }
    ad.innerHTML = ah;
  } catch (_) {
    T('Failed to load devices', 'e');
  }
}

async function createCameraFromDevice(deviceIndex) {
  try {
    let r = await A('POST', '/api/cameras', {
      name: 'USB Camera ' + deviceIndex,
      camera_type: 'usb',
      config: { device_index: deviceIndex },
    });
    if (r.ok) {
      T('USB camera created', 's');
      await D();
    } else {
      T(r.data && r.data.error ? r.data.error : 'Failed to create camera', 'e');
    }
  } catch (_) {
    T('Cannot reach server. Check it\'s running and reload.', 'e');
  }
}

// ============================================================
// ROUTER
// ============================================================
async function r() {
  spoll_stop();
  let route = R();

  switch (route.p) {
    case 'login':
      N(0);
      P('login');
      break;

    case 'stp':
      N(0);
      P('stp');
      break;

    case 'c':
      if (!S.a) { await i(); return; }
      await K(route.id);
      break;

    case 'st':
      if (!S.a) { await i(); return; }
      await M();
      break;

    case 'dv':
      if (!S.a) { await i(); return; }
      P('dv');
      N(1);
      await loadDevices();
      break;

    default:
      if (!S.a) { await i(); return; }
      await D();
      break;
  }
}

// ============================================================
// INIT
// ============================================================
async function i() {
  let s = await C();

  if (s.a) {
    S.a = 1;
    N(1);
    await D();
  } else if (s.s) {
    S.a = 0;
    N(0);
    P('stp');
    window.location.hash = '#stp';
  } else {
    S.a = 0;
    N(0);
    P('login');
    window.location.hash = '#login';
  }
}

// ============================================================
// UTILITY
// ============================================================
function E(s) {
  if (typeof s !== 'string') return '';
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function J(s) {
  if (typeof s !== 'string') return '';
  return s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}

// ============================================================

// ============================================================
// PASSWORD CHANGE (F6)
// ============================================================
async function cp() {
  let o = document.getElementById('cpw').value;
  let n = document.getElementById('npw').value;
  let c = document.getElementById('cnpw').value;
  if (!o || !n || !c) { T('All fields are required.', 'e'); return; }
  if (n !== c) { T('Passwords do not match.', 'e'); return; }
  if (n.length < 8) { T('Password must be at least 8 characters.', 'e'); return; }
  let btn = document.getElementById('cpbtn');
  btn.disabled = 1;
  btn.textContent = 'Changing...';
  try {
    let r = await A('POST', '/api/auth/reset', { old_password: o, new_password: n });
    if (r.ok) {
      T('Password changed. Please sign in again.', 's');
      setTimeout(() => { O(); }, 1500);
    } else {
      T(r.data && r.data.error ? r.data.error : 'Failed', 'e');
    }
  } catch (_) {
    T('Cannot reach server. Check it\'s running and reload.', 'e');
  } finally {
    btn.disabled = 0;
    btn.textContent = 'Change Password';
  }
}

// ============================================================
// CAMERA EDIT (F7)
// ============================================================
async function ec(id) {
  try {
    let r = await A('GET', '/api/cameras/' + id);
    if (!r.ok) { T('Failed to load camera', 'e'); return; }
    let c = r.data;
    S.editId = id;
    document.getElementById('mt').textContent = 'Edit Camera';
    document.getElementById('cn2').value = c.name || '';
    document.getElementById('cty').value = c.camera_type || '';
    document.getElementById('cc').value = c.config ? JSON.stringify(c.config, null, 2) : '';
    document.getElementById('mo').classList.remove('h');
  } catch (_) {
    T('Cannot reach server. Check it\'s running and reload.', 'e');
  }
}

// ============================================================
// PROTOCOL CONFIG (F8)
// ============================================================
async function lp() {
  ['onvif', 'gb28181', 'rtmp'].forEach(async function(p) {
    try {
      let r = await A('GET', '/api/protocols/' + p);
      if (r.ok && r.data) {
        let prefix = { onvif: 'onv-', gb28181: 'gb-', rtmp: 'rtmp-' }[p];
        let map = { enabled: 'enabled', port: 'port', device_name: 'device-name',
                    manufacturer: 'manufacturer', model: 'model', device_id: 'device-id',
                    server_ip: 'server-ip', server_port: 'server-port', url: 'url' };
        Object.keys(r.data).forEach(function(k) {
          let el = document.getElementById(prefix + (map[k] || k));
          if (el) el.value = r.data[k];
        });
      }
    } catch (_) {}
  });
}

async function sp(p) {
  let prefix = { onvif: 'onv-', gb28181: 'gb-', rtmp: 'rtmp-' }[p];
  let cfg = {};
  let map = { enabled: 'enabled', port: 'port', 'device-name': 'device_name',
              'device-id': 'device_id', 'server-ip': 'server_ip',
              'server-port': 'server_port', 'url': 'url' };
  document.querySelectorAll('[id^="' + prefix + '"]').forEach(function(el) {
    let key = map[el.id.slice(prefix.length)] || el.id.slice(prefix.length);
    cfg[key] = el.value;
  });
  try {
    let r = await A('PUT', '/api/protocols/' + p, cfg);
    if (r.ok) T(p.toUpperCase() + ' config saved', 's');
    else T(r.data && r.data.error ? r.data.error : 'Failed', 'e');
  } catch (_) {
    T('Cannot reach server. Check it\'s running and reload.', 'e');
  }
}

// ============================================================
// AUTO-REFRESH DASHBOARD (F11)
// ============================================================
function spoll() {
  if (pi) clearInterval(pi);
  pi = setInterval(async function() {
    if (window.location.hash !== '#db') { spoll_stop(); return; }
    try {
      let r = await A('GET', '/api/cameras');
      if (r.ok && Array.isArray(r.data)) {
        S.c = r.data;
        V();
      }
    } catch (_) {}
  }, 10000);
}

function spoll_stop() { if (pi) { clearInterval(pi); pi = null; } }

// EVENT BINDING
// ============================================================
document.addEventListener('DOMContentLoaded', function() {
  document.getElementById('lf').addEventListener('submit', L);
  document.getElementById('sf').addEventListener('submit', H);
  document.getElementById('lo').addEventListener('click', O);

  document.getElementById('mo').addEventListener('click', function(e) {
    if (e.target === this) cm();
  });
  document.getElementById('co').addEventListener('click', function(e) {
    if (e.target === this) cC();
  });

  document.addEventListener('keydown', function(e) {
    let mo = document.getElementById('mo'), co = document.getElementById('co');
    if (e.key === 'Escape') {
      if (!mo.classList.contains('h')) cm();
      if (!co.classList.contains('h')) cC();
    }
    if (e.key === 'Tab') {
      if (!mo.classList.contains('h')) ft(mo, e);
      else if (!co.classList.contains('h')) ft(co, e);
    }
  });

  document.getElementById('hbtn').addEventListener('click', function() {
    document.querySelector('.nl').classList.toggle('open');
  });
  document.querySelectorAll('.nl a').forEach(function(a) {
    a.addEventListener('click', function() {
      document.querySelector('.nl').classList.remove('open');
    });
  });

  window.addEventListener('hashchange', r);
  setupDeviceSelector();
  i();
});

// Expose functions for onclick handlers in HTML
window.G = G;
window.wa = wa;
window.cm = cm;
window.sc = sc;
window.x = x;
window.cC = cC;
window.cD = cD;
window.Y = Y;
window.w = w;
window.cr = cr;
window.Q = Q;
window.A = A;
window.copyUrl = copyUrl;
window.createCameraFromDevice = createCameraFromDevice;
window.cp = cp;
window.ec = ec;
window.sp = sp;

})();
