// Settings view: recursive config editor over GET/PUT /api/config (SPEC §5).
// Device schemas differ — the editor renders whatever document it receives.

import { api, refreshCapabilities } from './api.js';
import { store } from './store.js';
import { $, toast, setBtnLoading } from './ui.js';
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

export async function loadConfig() {
  const form = $('config-form'), loading = $('config-loading');
  form.classList.add('hidden');
  loading.classList.remove('hidden');
  store.configDirty = false;
  updateSaveState();
  const r = await api.get('/api/config');
  if (r.ok) {
    store.config = r.data;
    buildForm(store.config, form, '');
    form.classList.remove('hidden');
  } else {
    toast(t('loadError'), 'error');
  }
  loading.classList.add('hidden');
  renderApplyBanner();
}

function renderApplyBanner() {
  const banner = $('config-apply-banner');
  if (!banner) return;
  const apply = (store.caps && store.caps.config_apply) || {};
  const restartish = apply.default === 'restart' ||
    Object.values(apply.sections || {}).includes('restart');
  banner.classList.toggle('hidden', !restartish);
}

function buildForm(obj, parent, prefix) {
  parent.innerHTML = '';
  const leaf = [], nest = [];
  for (const [k, v] of Object.entries(obj)) {
    (v && typeof v === 'object' ? nest : leaf).push([k, v]);
  }
  for (const [key, val] of leaf) {
    const p = prefix ? prefix + '.' + key : key;
    const row = document.createElement('div');
    row.className = 'config-field';
    row.dataset.cfg = p;
    const typ = typeof val;
    if (typ === 'boolean') {
      row.innerHTML = '<div class="field-row"><input type="checkbox" id="cf-' + p + '" ' + (val ? 'checked' : '') + ' data-cfg="' + p + '"><label for="cf-' + p + '">' + cfgLabel(p, key) + '</label></div>';
    } else if (typ === 'number') {
      const c = CONSTRAINTS[p] || {};
      const attrs = (c.min !== undefined ? ' min="' + c.min + '"' : '') + (c.max !== undefined ? ' max="' + c.max + '"' : '');
      row.innerHTML = '<label for="cf-' + p + '">' + cfgLabel(p, key) + '</label><input type="number" id="cf-' + p + '" value="' + val + '" step="any"' + attrs + ' data-cfg="' + p + '">';
    } else {
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
    const hdr = document.createElement('button');
    hdr.type = 'button';
    hdr.className = 'config-section-title';
    hdr.setAttribute('aria-expanded', 'true');
    hdr.innerHTML = esc(cfgLabel(p, key)) + '<span class="chev">' + icon('chevron-down', 15) + '</span>';
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
    const parts = node.getAttribute('data-cfg').split('.');
    let val;
    if (node.type === 'checkbox') val = node.checked;
    else if (node.type === 'number') {
      const n = parseFloat(node.value);
      val = (node.value === '' || isNaN(n)) ? null : n;
    } else val = maybeNum(node.value);
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

function markDirty() {
  store.configDirty = true;
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
  } else {
    toast(r.message || r.error || t('saveError'), 'error');
  }
  saveInProgress = false;
  setBtnLoading(btn, false);
  updateSaveState();
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
  window.addEventListener('beforeunload', (e) => {
    if (store.configDirty) { e.preventDefault(); e.returnValue = ''; }
  });
}
