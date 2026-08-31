// Status view: /api/status poll + camera info + connection badges.

import { api } from './api.js';
import { store, hasCap } from './store.js';
import { $, esc } from './ui.js';
import { t } from './i18n.js';

let pollTimer = null;

export async function checkApi() {
  const r = await api.get('/api/health');
  const b = $('api-badge'), l = $('api-label');
  if (b) b.className = 'status-badge ' + (r.ok ? 'online' : 'offline');
  if (l) l.textContent = t(r.ok ? 'connected' : 'disconnected');
}

export async function refreshStatus() {
  const r = await api.get('/api/status');
  if (!r.ok) return;
  const s = r.data || {};
  const info = $('device-info');
  if (info) {
    const rows = [
      [t('deviceName'), s.device_name],
      [t('model'), s.model],
      [t('vendor'), s.vendor],
      [t('firmware'), s.firmware],
      [t('uptime'), fmtUptime(s.uptime)],
    ];
    const extras = [];
    if (s.recording !== undefined) extras.push(['recording', s.recording ? '\u2713' : '\u2014']);
    if (s.gb28181 !== undefined) extras.push(['GB28181', s.gb28181 ? '\u2713' : '\u2014']);
    info.innerHTML = rows.concat(extras).map(([label, val]) =>
      '<div class="reading"><span class="reading-label">' + esc(label) + '</span><span class="reading-val">' + esc(val !== undefined && val !== null ? String(val) : '-') + '</span></div>'
    ).join('');
  }
  renderCameraInfo();
}

async function renderCameraInfo() {
  const info = $('camera-info');
  if (!info) return;
  const cam = store.cameras[0];
  if (!cam) {
    info.innerHTML = '<div class="reading"><span class="reading-label">' + t('noCamera') + '</span></div>';
    return;
  }
  const rows = [
    [t('cameraName'), cam.name],
    [t('statusOnline'), cam.status],
    [t('resolution'), cam.resolution],
    [t('fps'), cam.fps],
  ];
  info.innerHTML = rows.map(([label, val]) =>
    '<div class="reading"><span class="reading-label">' + esc(label) + '</span><span class="reading-val">' + esc(val !== undefined && val !== null ? String(val) : '-') + '</span></div>'
  ).join('');
}

function fmtUptime(secs) {
  if (secs === undefined || secs === null) return '-';
  const s = Number(secs);
  if (isNaN(s)) return String(secs);
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return d + 'd ' + h + 'h';
  if (h > 0) return h + 'h ' + m + 'm';
  return m + 'm ' + (s % 60) + 's';
}

export function initStatus() {
  // Hide the events badge row when the device has no SSE events.
  const evRow = $('events-row');
  if (evRow) evRow.classList.toggle('hidden', !hasCap('events') || !((store.caps.events || []).length));
}

export function startStatusPolling() {
  if (pollTimer) return;
  pollTimer = setInterval(() => {
    if (!$('view-status').classList.contains('active')) return;
    checkApi();
    refreshStatus();
  }, 10000);
}
