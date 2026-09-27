// Detection zones (SPEC appendix A #21, extension: zones): intrusion and
// line-cross zones in video-pixel coordinates.
//
// Two surfaces:
//  - live overlay: zone outlines on top of the stream (same contain-fit
//    geometry the AI detection overlay uses);
//  - editor dialog: snapshot backdrop, click to place points, commit a
//    zone, delete zones, PUT the whole list (`applied: "immediate"`).

import { api } from './api.js';
import { store, cameraId, hasCap } from './store.js';
import { $, toast } from './ui.js';
import { t } from './i18n.js';

let zonesCache = [];

// ─── Live overlay ───────────────────────────────────────────────────

/// Fetch zones for a camera and redraw the overlay. Called on camera
/// switch and after an editor save.
export async function refreshZones() {
  zonesCache = [];
  if (!hasCap('zones')) { renderZonesOverlay(); return; }
  const id = cameraId();
  const res = await api.get(`/api/cameras/${encodeURIComponent(id)}/zones`);
  if (res.ok && res.data && Array.isArray(res.data.zones)) zonesCache = res.data.zones;
  renderZonesOverlay();
}

/// Draw zone outlines on the live view (canvas #zones-overlay). Zones are
/// stored in video-pixel coordinates; the mapping mirrors the detection
/// overlay's contain-fit + letterbox math.
export function renderZonesOverlay() {
  const canvas = $('zones-overlay');
  const video = $('stream-video');
  const wrapper = document.querySelector('.stream-wrapper');
  if (!canvas || !video || !wrapper) return;
  const media = video.classList.contains('hidden') ? $('stream-img') : video;
  const ctx = canvas.getContext('2d');
  const dpr = window.devicePixelRatio || 1;
  const w = wrapper.clientWidth, h = wrapper.clientHeight;
  if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
  }
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  if (!zonesCache.length) return;
  const vw = (media && (media.videoWidth || media.naturalWidth)) || 0;
  const vh = (media && (media.videoHeight || media.naturalHeight)) || 0;
  if (!vw || !vh) return;
  const scale = Math.min(w / vw, h / vh);
  const ox = (w - vw * scale) / 2, oy = (h - vh * scale) / 2;

  ctx.lineWidth = 2;
  ctx.font = '11px monospace';
  for (const z of zonesCache) {
    const pts = z.points || [];
    if (!pts.length) continue;
    const line = z.kind === 'line_cross';
    ctx.strokeStyle = line ? '#e0a800' : '#3aa0ff';
    ctx.setLineDash(line ? [8, 6] : []);
    ctx.beginPath();
    pts.forEach(([px, py], i) => {
      const x = ox + px * scale, y = oy + py * scale;
      if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
    });
    if (!line) ctx.closePath();
    ctx.stroke();
    ctx.setLineDash([]);
    if (z.name) {
      const x = ox + pts[0][0] * scale, y = oy + pts[0][1] * scale - 6;
      ctx.fillStyle = 'rgba(0,0,0,.55)';
      const tw = ctx.measureText(z.name).width;
      ctx.fillRect(x - 2, y - 11, tw + 6, 14);
      ctx.fillStyle = '#d8e6ff';
      ctx.fillText(z.name, x + 1, y);
    }
  }
}

// ─── Editor ─────────────────────────────────────────────────────────

let draft = [];            // [[x,y]…] in frame pixels
let editZones = [];        // working copy while the dialog is open
let editNatural = null;    // {w,h} of the snapshot backdrop
let editMapping = null;    // contain-fit mapping for click → frame px

/// Show the zones toolbar button only on zones-capable devices.
export function updateZonesVisibility() {
  const btn = $('btn-zones-edit');
  if (btn) btn.classList.toggle('hidden', !hasCap('zones'));
}

export function initZones() {
  const btn = $('btn-zones-edit');
  if (!btn) return;
  btn.addEventListener('click', openZonesEditor);
  updateZonesVisibility();

  const close = $('zones-close');
  if (close) close.addEventListener('click', closeZonesEditor);
  const overlay = $('zones-editor');
  if (overlay) overlay.addEventListener('click', (e) => { if (e.target === overlay) closeZonesEditor(); });

  $('zones-undo') && $('zones-undo').addEventListener('click', () => { draft.pop(); drawEditor(); });
  $('zones-clear') && $('zones-clear').addEventListener('click', () => { draft = []; drawEditor(); });
  $('zones-add') && $('zones-add').addEventListener('click', commitDraft);
  $('zones-save') && $('zones-save').addEventListener('click', saveZones);
  const canvas = $('zones-edit-canvas');
  if (canvas) {
    canvas.addEventListener('click', (e) => {
      if (!editMapping) return;
      const r = canvas.getBoundingClientRect();
      const cx = e.clientX - r.left, cy = e.clientY - r.top;
      const { scale, ox, oy, w: vw, h: vh } = editMapping;
      const px = Math.round((cx - ox) / scale);
      const py = Math.round((cy - oy) / scale);
      if (px < 0 || py < 0 || px >= vw || py >= vh) return;
      const kindSel = $('zones-kind');
      const isLine = kindSel && kindSel.value === 'line_cross';
      if (isLine && draft.length >= 2) draft.shift(); // tripwire: keep the last two
      draft.push([px, py]);
      drawEditor();
    });
  }
  window.addEventListener('resize', renderZonesOverlay);
}

async function openZonesEditor() {
  const overlayEl = $('zones-editor');
  if (!overlayEl) return;
  const id = cameraId();
  const res = await api.get(`/api/cameras/${encodeURIComponent(id)}/zones`);
  if (!res.ok || !res.data || !Array.isArray(res.data.zones)) {
    // Do not open an empty editor on a failed load — saving it would
    // wipe the camera's zones (e.g. the camera just went offline).
    toast(t('zonesLoadFailed'), 'error');
    return;
  }
  editZones = JSON.parse(JSON.stringify(res.data.zones));
  draft = [];
  overlayEl.classList.remove('hidden');
  await loadBackdrop(id);
  drawEditor();
}

async function loadBackdrop(id) {
  const canvas = $('zones-edit-canvas');
  if (!canvas) return;
  // Frame dimensions: the live media element knows them even when the
  // snapshot has not loaded yet.
  const video = $('stream-video');
  const media = video && !video.classList.contains('hidden') ? video : $('stream-img');
  const vw = (media && (media.videoWidth || media.naturalWidth)) || 1280;
  const vh = (media && (media.videoHeight || media.naturalHeight)) || 720;
  editNatural = { w: vw, h: vh };
  const img = new Image();
  img.onload = () => {
    if (img.naturalWidth) editNatural = { w: img.naturalWidth, h: img.naturalHeight };
    drawEditor();
  };
  img.src = `/api/cameras/${encodeURIComponent(id)}/snapshot?_=${Date.now()}`;
  canvas._backdrop = img;
  drawEditor();
}

function drawEditor() {
  const canvas = $('zones-edit-canvas');
  if (!canvas || !editNatural) return;
  const wrap = canvas.parentElement;
  const maxW = Math.max(320, wrap.clientWidth - 4);
  const maxH = Math.min(560, Math.max(240, window.innerHeight - 320));
  const { w: vw, h: vh } = editNatural;
  const scale = Math.min(maxW / vw, maxH / vh);
  const dw = Math.round(vw * scale), dh = Math.round(vh * scale);
  canvas.width = dw; canvas.height = dh;
  canvas.style.width = dw + 'px'; canvas.style.height = dh + 'px';
  editMapping = { scale, ox: 0, oy: 0, w: vw, h: vh };
  const ctx = canvas.getContext('2d');
  ctx.clearRect(0, 0, dw, dh);
  // backdrop: snapshot pixels if loaded (re-read via the img element kept
  // on the canvas), else a dark grid so points stay visible.
  const img = canvas._backdrop;
  if (img && img.complete && img.naturalWidth) ctx.drawImage(img, 0, 0, dw, dh);
  else { ctx.fillStyle = '#10141c'; ctx.fillRect(0, 0, dw, dh); }

  const drawZone = (z, dim) => {
    const pts = z.points || [];
    if (!pts.length) return;
    const line = z.kind === 'line_cross';
    ctx.globalAlpha = dim ? 0.5 : 1;
    ctx.lineWidth = 2;
    ctx.strokeStyle = line ? '#e0a800' : '#3aa0ff';
    ctx.setLineDash(line ? [8, 6] : []);
    ctx.beginPath();
    pts.forEach(([px, py], i) => {
      const x = px * scale, y = py * scale;
      if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
    });
    if (!line) ctx.closePath();
    ctx.stroke();
    ctx.setLineDash([]);
    if (z.name) {
      ctx.fillStyle = 'rgba(0,0,0,.6)';
      const tw = ctx.measureText(z.name).width;
      ctx.fillRect(pts[0][0] * scale - 2, pts[0][1] * scale - 15, tw + 6, 14);
      ctx.fillStyle = '#d8e6ff';
      ctx.font = '11px monospace';
      ctx.fillText(z.name, pts[0][0] * scale + 1, pts[0][1] * scale - 5);
    }
    ctx.globalAlpha = 1;
  };
  for (const z of editZones) drawZone(z, false);
  // draft in progress
  if (draft.length) {
    ctx.strokeStyle = '#00e0b0';
    ctx.lineWidth = 2;
    ctx.setLineDash([5, 4]);
    ctx.beginPath();
    draft.forEach(([px, py], i) => {
      const x = px * scale, y = py * scale;
      if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
    });
    ctx.stroke();
    ctx.setLineDash([]);
    for (const [px, py] of draft) {
      ctx.fillStyle = '#00e0b0';
      ctx.beginPath();
      ctx.arc(px * scale, py * scale, 3.5, 0, Math.PI * 2);
      ctx.fill();
    }
  }
  renderZoneList();
}

function renderZoneList() {
  const ul = $('zones-list');
  if (!ul) return;
  ul.innerHTML = '';
  editZones.forEach((z, i) => {
    const li = document.createElement('li');
    const kindTxt = z.kind === 'line_cross' ? t('zoneKindLine') : t('zoneKindIntrusion');
    li.textContent = `${z.name || ('#' + (i + 1))} · ${kindTxt}`;
    const del = document.createElement('button');
    del.type = 'button';
    del.className = 'tool-btn danger';
    del.textContent = '✕';
    del.title = t('delete');
    del.addEventListener('click', () => { editZones.splice(i, 1); drawEditor(); });
    li.appendChild(del);
    ul.appendChild(li);
  });
}

function commitDraft() {
  const kindSel = $('zones-kind');
  const kind = (kindSel && kindSel.value) || 'intrusion';
  const need = kind === 'line_cross' ? 2 : 3;
  if (draft.length < need) {
    toast(t('zoneNeedPoints', { n: need }), 'error');
    return;
  }
  const nameEl = $('zones-name');
  const name = (nameEl && nameEl.value.trim()) || '';
  if (!name) { toast(t('zoneNeedName'), 'error'); return; }
  const dwell = Number($('zones-dwell') && $('zones-dwell').value) || 0;
  editZones.push({ name, kind, points: draft.slice(), dwell_secs: dwell });
  draft = [];
  if (nameEl) nameEl.value = '';
  drawEditor();
}

async function saveZones() {
  const id = cameraId();
  const res = await api.put(`/api/cameras/${encodeURIComponent(id)}/zones`, editZones);
  if (!res.ok) { toast(t('saveFailed'), 'error'); return; }
  toast(t('zonesSaved'), 'success');
  closeZonesEditor();
  refreshZones();
}

function closeZonesEditor() {
  const overlayEl = $('zones-editor');
  if (overlayEl) overlayEl.classList.add('hidden');
}
