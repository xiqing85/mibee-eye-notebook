// Settings view: recursive config editor over GET/PUT /api/config (SPEC §5).
// Device schemas differ — the editor renders whatever document it receives.

import { api, refreshCapabilities } from './api.js';
import { store, hasCap } from './store.js';
import { $, toast, setBtnLoading, confirmDlg } from './ui.js';
import { t, cfgLabel } from './i18n.js';
import { icon } from './icons.js';
import { applyTransform } from './live.js';
import { initImaging } from './imaging.js';

const ENUMS = {
  'camera.mode': ['mtxrpicam', 'rtsp'],
  'camera.codec': ['h264', 'h265'],
  'logging.level': ['debug', 'info', 'warn', 'error'],
};
const CONSTRAINTS = {
  'camera.width': { min: 64, max: 4608, required: true },
  'camera.height': { min: 64, max: 2592, required: true },
  'camera.fps': { min: 1, max: 90, required: true },
  'camera.bitrate': { min: 100000, max: 50000000, required: true },
  'onvif.port': { min: 1, max: 65535, required: false },
  'rtsp.port': { min: 1, max: 65535, required: false },
  'web.port': { min: 1, max: 65535, required: false },
};
const PASSWORD_FIELDS = new Set(['rtsp.password', 'onvif.password', 'gb28181.password', 'web.password']);
// GB/T 28181 device/channel IDs are exactly 20 digits.
const GB28181_ID_FIELDS = new Set(['gb28181.device_id', 'gb28181.channel_id']);

let saveInProgress = false;
/// Config paths whose document value is a string. collectConfig must send
/// them back verbatim: numeric-looking strings (20-digit GB/T 28181 SIP IDs,
/// phone numbers) would otherwise be converted to floats — losing precision
/// above 2^53 and getting the PUT rejected by devices (`expected a string`).
const stringFields = new Set();
/// Config paths whose document value is an array (e.g. cpu_cores). Rendered
/// read-only and omitted from the PUT: deepMerge keeps the stored value, and
/// sending an index-keyed object in its place would be rejected
/// (`expected a sequence`).
const arrayFields = new Set();
/// Top-level config sections the user has edited since load/save — drives
/// the post-save restart banner (SPEC §5: restart sections need a service
/// restart to apply; the UI surfaces a one-click restart when supported).
const dirtySections = new Set();

/// Apply semantics of a config section from capabilities.config_apply
/// (SPEC §3.1): sections override the default; nested paths fall back to
/// their top-level section, then the device default.
function sectionApply(path) {
  const ca = (store.caps && store.caps.config_apply) || {};
  const s = ca.sections || {};
  return s[path] || s[path.split('.')[0]] || ca.default || 'immediate';
}

export async function loadConfig() {
  const form = $('config-form'), loading = $('config-loading');
  form.classList.add('hidden');
  loading.classList.remove('hidden');
  store.configDirty = false;
  updateSaveState();
  const r = await api.get('/api/config');
  if (r.ok) {
    store.config = r.data;
    stringFields.clear();
    arrayFields.clear();
    dirtySections.clear();
    buildForm(store.config, form, '');
    form.classList.remove('hidden');
  } else {
    toast(t('loadError'), 'error');
  }
  loading.classList.add('hidden');
  renderApplyBanner();
}

/// Banner under the settings header: shows apply semantics, and — when the
/// device supports it (capabilities.restart, SPEC §5.1) — a one-click
/// restart button. `changed` lists restart sections edited in this save
/// cycle, upgrading the copy from generic to actionable.
function renderApplyBanner(changed = null) {
  const banner = $('config-apply-banner');
  if (!banner) return;
  const apply = (store.caps && store.caps.config_apply) || {};
  const restartish = apply.default === 'restart' ||
    Object.values(apply.sections || {}).includes('restart');
  banner.classList.toggle('hidden', !restartish);
  if (!restartish) return;
  const btn = $('btn-restart-device');
  if (btn) btn.classList.toggle('hidden', !hasCap('restart'));
  const label = banner.querySelector('[data-i18n=applyRestart]');
  if (label) {
    label.textContent = changed && changed.length
      ? t('savedNeedRestart', { sections: changed.join(', ') })
      : t('applyRestart');
  }
}

function buildForm(obj, parent, prefix) {
  parent.innerHTML = '';
  const leaf = [], nest = [];
  for (const [k, v] of Object.entries(obj)) {
    (v && typeof v === 'object' && !Array.isArray(v) ? nest : leaf).push([k, v]);
  }
  for (const [key, val] of leaf) {
    const p = prefix ? prefix + '.' + key : key;
    const row = document.createElement('div');
    row.className = 'config-field';
    row.dataset.cfg = p;
    if (Array.isArray(val)) {
      arrayFields.add(p);
      row.innerHTML = '<label for="cf-' + p + '">' + cfgLabel(p, key) + '</label>'
        + '<input type="text" id="cf-' + p + '" value="' + esc(JSON.stringify(val)) + '" data-cfg="' + p + '" disabled title="列表字段暂不支持在线编辑">';
      parent.appendChild(row);
      continue;
    }
    const typ = typeof val;
    if (typ === 'boolean') {
      row.innerHTML = '<div class="field-row"><input type="checkbox" id="cf-' + p + '" ' + (val ? 'checked' : '') + ' data-cfg="' + p + '"><label for="cf-' + p + '">' + cfgLabel(p, key) + '</label></div>';
    } else if (typ === 'number') {
      const c = CONSTRAINTS[p] || {};
      const attrs = (c.min !== undefined ? ' min="' + c.min + '"' : '') + (c.max !== undefined ? ' max="' + c.max + '"' : '');
      row.innerHTML = '<label for="cf-' + p + '">' + cfgLabel(p, key) + '</label><input type="number" id="cf-' + p + '" value="' + val + '" step="any"' + attrs + ' data-cfg="' + p + '">';
    } else {
      stringFields.add(p);
      const en = ENUMS[p];
      if (en) {
        row.innerHTML = '<label for="cf-' + p + '">' + cfgLabel(p, key) + '</label><select id="cf-' + p + '" data-cfg="' + p + '">' + en.map((o) => '<option ' + (o === val ? 'selected' : '') + '>' + o + '</option>').join('') + '</select>';
      } else if (PASSWORD_FIELDS.has(p)) {
        row.innerHTML = '<label for="cf-' + p + '">' + cfgLabel(p, key) + '</label><div class="password-wrap"><input type="password" id="cf-' + p + '" value="' + esc(String(val)) + '" data-cfg="' + p + '" autocomplete="new-password"><button type="button" class="password-toggle" data-target="cf-' + p + '" aria-label="' + t('showPassword') + '">' + icon('eye', 18) + '</button></div>';
      } else {
        const idAttrs = GB28181_ID_FIELDS.has(p) ? ' maxlength="20" placeholder="' + t('gb28181.id20Placeholder') + '"' : '';
        row.innerHTML = '<label for="cf-' + p + '">' + cfgLabel(p, key) + '</label><input type="text" id="cf-' + p + '" value="' + esc(String(val)) + '"' + idAttrs + ' data-cfg="' + p + '">';
      }
    }
    parent.appendChild(row);
  }
  for (const [key, val] of nest) {
    const p = prefix ? prefix + '.' + key : key;
    const sec = document.createElement('div');
    sec.className = 'config-section';
    const apply = sectionApply(p);
    const badge = '<span class="section-apply-badge apply-' + apply + '">'
      + t(apply === 'restart' ? 'applyRestartSec' : 'applyImmediateSec') + '</span>';
    const hdr = document.createElement('button');
    hdr.type = 'button';
    hdr.className = 'config-section-title';
    hdr.setAttribute('aria-expanded', 'true');
    hdr.innerHTML = esc(cfgLabel(p, key)) + badge + '<span class="chev">' + icon('chevron-down', 15) + '</span>';
    hdr.addEventListener('click', () => {
      const collapsed = sec.classList.toggle('collapsed');
      hdr.setAttribute('aria-expanded', String(!collapsed));
    });
    const body = document.createElement('div');
    body.className = 'config-section-body';
    sec.appendChild(hdr);
    sec.appendChild(body);
    parent.appendChild(sec);
    buildForm(val, body, p);
  }
}

function esc(s) {
  return String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
}

function validateConfig() {
  let allOk = true;
  let firstInvalid = null;
  document.querySelectorAll('#config-form [data-cfg]').forEach((node) => {
    const p = node.getAttribute('data-cfg');
    const field = node.closest('.config-field');
    if (!field) return;
    let ok = true;
    const c = CONSTRAINTS[p];
    if (node.type === 'number') {
      const raw = node.value.trim();
      const n = parseFloat(raw);
      if (raw === '' || isNaN(n)) ok = !(c && c.required);
      else if (c) {
        if (c.min !== undefined && n < c.min) ok = false;
        if (c.max !== undefined && n > c.max) ok = false;
      }
    }
    field.classList.toggle('invalid', !ok);
    if (!ok) { allOk = false; if (!firstInvalid) firstInvalid = node; }
  });
  return { ok: allOk, firstInvalid };
}

function collectConfig() {
  const out = {};
  document.querySelectorAll('#config-form [data-cfg]').forEach((node) => {
    const p = node.getAttribute('data-cfg');
    if (arrayFields.has(p)) return; // omitted → deepMerge keeps the stored array
    const parts = p.split('.');
    let val;
    if (node.type === 'checkbox') val = node.checked;
    else if (node.type === 'number') {
      const n = parseFloat(node.value);
      val = (node.value === '' || isNaN(n)) ? null : n;
    } else if (stringFields.has(p)) val = node.value;
    else val = maybeNum(node.value);
    let o = out;
    for (let i = 0; i < parts.length - 1; i++) {
      if (!o[parts[i]] || typeof o[parts[i]] !== 'object') o[parts[i]] = {};
      o = o[parts[i]];
    }
    o[parts[parts.length - 1]] = val;
  });
  return out;
}

function maybeNum(v) {
  const n = Number(v);
  return (v === '' || isNaN(n)) ? v : n;
}

function markDirty(ev) {
  store.configDirty = true;
  const node = ev && ev.target;
  if (node && node.dataset && node.dataset.cfg) {
    dirtySections.add(node.dataset.cfg.split('.')[0]);
  }
  // Always re-evaluate: fixing an invalid field must re-enable Save.
  updateSaveState();
}

function updateSaveState() {
  const btn = $('save-config');
  if (!btn) return;
  const indicator = $('unsaved-indicator');
  const { ok } = validateConfig();
  btn.disabled = !store.configDirty || !ok;
  if (indicator) indicator.classList.toggle('hidden', !store.configDirty);
}

export async function handleSave() {
  if (saveInProgress) return;
  const { ok, firstInvalid } = validateConfig();
  if (!ok) {
    toast(t('validationError'), 'error');
    if (firstInvalid) firstInvalid.focus();
    return;
  }
  saveInProgress = true;
  const btn = $('save-config');
  setBtnLoading(btn, true);
  const merged = deepMerge(store.config || {}, collectConfig());
  const r = await api.put('/api/config', merged);
  if (r.ok) {
    store.config = merged;
    store.configDirty = false;
    toast(t('saved'), 'success');
    applyTransform();
    // Capabilities may have flipped (e.g. recording enabled) — refresh.
    await refreshCapabilities();
    initImaging();
    // Point the user at the restart entry when edited sections need it.
    const changedRestart = [...dirtySections].filter((s) => sectionApply(s) === 'restart');
    renderApplyBanner(changedRestart);
    dirtySections.clear();
  } else {
    toast(r.message || r.error || t('saveError'), 'error');
  }
  saveInProgress = false;
  setBtnLoading(btn, false);
  updateSaveState();
}

/// One-click service restart (SPEC §5.1): confirm → POST → poll the public
/// /api/health until the service is back → hard reload (in-memory sessions
/// die with the old process, so a clean reload is the only correct landing).
async function restartDevice() {
  const ok = await confirmDlg({
    message: t('restartConfirm'),
    okText: t('restartNow'),
    cancelText: t('cancel'),
  });
  if (!ok) return;
  const overlay = $('restart-overlay');
  if (overlay) overlay.classList.remove('hidden');
  try {
    await api.post('/api/system/restart');
  } catch (_) { /* connection resets as the process exits — expected */ }
  const deadline = Date.now() + 90000;
  await new Promise((r) => setTimeout(r, 3000));
  for (;;) {
    let up = false;
    try {
      const resp = await fetch('/api/health', { credentials: 'same-origin' });
      up = resp.ok;
    } catch (_) { up = false; }
    if (up) { window.location.reload(); return; }
    if (Date.now() > deadline) break;
    await new Promise((r) => setTimeout(r, 2000));
  }
  const label = $('restart-overlay-label');
  if (label) label.textContent = t('restartTimeout');
}

export function deepMerge(a, b) {
  const out = { ...a };
  for (const [k, v] of Object.entries(b)) {
    if (v && typeof v === 'object' && !Array.isArray(v) && out[k] && typeof out[k] === 'object' && !Array.isArray(out[k])) {
      out[k] = deepMerge(out[k], v);
    } else out[k] = v;
  }
  return out;
}

export function initSettings() {
  const form = $('config-form');
  if (form) {
    form.addEventListener('input', markDirty);
    form.addEventListener('change', markDirty);
  }
  const save = $('save-config');
  if (save) save.addEventListener('click', handleSave);
  const restartBtn = $('btn-restart-device');
  if (restartBtn) restartBtn.addEventListener('click', restartDevice);
  window.addEventListener('beforeunload', (e) => {
    if (store.configDirty) { e.preventDefault(); e.returnValue = ''; }
  });
}
