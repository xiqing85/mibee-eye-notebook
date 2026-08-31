// PTZ panel (extension: ptz — virtual or real). Normalized absolute moves,
// press-and-hold D-pad, zoom slider, keyboard arrows.

import { api } from './api.js';
import { store, hasCap } from './store.js';
import { $ } from './ui.js';
import { t } from './i18n.js';

const ptz = { pan: 0.5, tilt: 0.5, zoom: 1.0 };

function deg(v) { return Math.round(((v || 0.5) - 0.5) * 180); }

export async function fetchPtz() {
  if (!hasCap('ptz')) return;
  const r = await api.get('/api/ptz/status');
  if (r.ok) { Object.assign(ptz, r.data); renderPtz(); }
}

async function move(dPan, dTilt, dZoom) {
  const pos = { ...ptz };
  if (dPan !== undefined) pos.pan = clamp(pos.pan + dPan, 0, 1);
  if (dTilt !== undefined) pos.tilt = clamp(pos.tilt + dTilt, 0, 1);
  if (dZoom !== undefined) pos.zoom = clamp(pos.zoom + dZoom, 0.5, 5);
  const r = await api.post('/api/ptz/move', pos);
  if (r.ok) { Object.assign(ptz, pos); renderPtz(); }
}

function clamp(v, min, max) { return Math.max(min, Math.min(max, v)); }

function renderPtz() {
  const panDeg = deg(ptz.pan) + '\u00b0';
  const tiltDeg = deg(ptz.tilt) + '\u00b0';
  const zoomVal = (ptz.zoom || 1.0).toFixed(1);
  const set = (id, v) => { const n = $(id); if (n) n.textContent = v; };
  set('ptz-pan', panDeg); set('ptz-tilt', tiltDeg); set('ptz-zoom', zoomVal);
  set('s-pan', panDeg); set('s-tilt', tiltDeg); set('s-zoom', zoomVal);
  const slider = $('zoom-slider');
  if (slider) slider.value = ptz.zoom || 1.0;
  const zv = $('zoom-value');
  if (zv) zv.textContent = zoomVal;
}

export function updatePtzVisibility() {
  const show = hasCap('ptz') && store.ptzEnabled;
  const panel = $('ptz-panel');
  if (panel) panel.classList.toggle('hidden', !show);
  const card = $('ptz-status-card');
  if (card) card.classList.toggle('hidden', !show);
}

export function initPtz() {
  if (!hasCap('ptz')) return;
  document.querySelectorAll('.ptz-btn').forEach((btn) => {
    let interval = null;
    let firedByPointer = false;
    const fire = () => {
      switch (btn.dataset.dir) {
        case 'up': move(0, -0.1); break;
        case 'down': move(0, 0.1); break;
        case 'left': move(-0.1, 0); break;
        case 'right': move(0.1, 0); break;
        case 'home': move(0.5 - ptz.pan, 0.5 - ptz.tilt, 1.0 - ptz.zoom); break;
      }
    };
    btn.addEventListener('pointerdown', () => {
      firedByPointer = true;
      fire();
      interval = setInterval(fire, 300);
    });
    for (const ev of ['pointerup', 'pointerleave', 'pointercancel']) {
      btn.addEventListener(ev, () => clearInterval(interval));
    }
    btn.addEventListener('click', () => {
      if (firedByPointer) { firedByPointer = false; return; }
      fire();
    });
    btn.addEventListener('contextmenu', (e) => e.preventDefault());
  });

  const slider = $('zoom-slider');
  if (slider) {
    slider.addEventListener('input', (e) => {
      const zv = $('zoom-value');
      if (zv) zv.textContent = parseFloat(e.target.value).toFixed(1);
    });
    slider.addEventListener('change', (e) => move(undefined, undefined, parseFloat(e.target.value) - ptz.zoom));
  }

  document.addEventListener('keydown', (e) => {
    if (!store.ptzEnabled) return;
    const preview = $('view-preview');
    if (!preview || !preview.classList.contains('active')) return;
    const tag = (document.activeElement && document.activeElement.tagName) || '';
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
    switch (e.key) {
      case 'ArrowUp': e.preventDefault(); move(0, -0.05); break;
      case 'ArrowDown': e.preventDefault(); move(0, 0.05); break;
      case 'ArrowLeft': e.preventDefault(); move(-0.05, 0); break;
      case 'ArrowRight': e.preventDefault(); move(0.05, 0); break;
      case 'Home': e.preventDefault(); move(0.5 - ptz.pan, 0.5 - ptz.tilt, 1.0 - ptz.zoom); break;
      case '+': case '=': e.preventDefault(); move(undefined, undefined, 0.1); break;
      case '-': case '_': e.preventDefault(); move(undefined, undefined, -0.1); break;
    }
  });

  const toggle = $('ptz-toggle');
  if (toggle) {
    toggle.checked = store.ptzEnabled;
    toggle.addEventListener('change', () => {
      store.ptzEnabled = toggle.checked;
      localStorage.setItem('mibee_ptz', store.ptzEnabled ? '1' : '0');
      updatePtzVisibility();
      if (store.ptzEnabled) fetchPtz();
    });
  }
  updatePtzVisibility();
}

export function handlePtzEvent(payload) {
  if (payload && payload.pan !== undefined) {
    ptz.pan = payload.pan; ptz.tilt = payload.tilt; ptz.zoom = payload.zoom;
    renderPtz();
  }
}
