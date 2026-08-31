// Cameras view (multi_camera only): grid of tiles with live thumbnails,
// start/stop (camera_control) and delete (camera_management).

import { api } from './api.js';
import { store, setCameraId, hasCap } from './store.js';
import { $, el, esc, toast } from './ui.js';
import { t } from './i18n.js';
import { showView } from './main.js';

const tileTimers = new Map();

// SPEC §4 camera status vocabulary: devices without camera_control report
// online/offline; controllable devices report running/stopped/idle.
const ACTIVE_STATUSES = ['online', 'running'];
const isActive = (cam) => ACTIVE_STATUSES.includes(cam && cam.status);

export async function refreshCameras() {
  const r = await api.get('/api/cameras');
  if (r.ok) store.cameras = r.data || [];
  return store.cameras;
}

export async function renderCameras() {
  const grid = $('cameras-grid');
  if (!grid) return;
  for (const timer of tileTimers.values()) clearInterval(timer);
  tileTimers.clear();
  grid.innerHTML = '';

  if (store.cameras.length === 0) {
    grid.appendChild(el('div', { className: 'empty-state', textContent: t('noCameras') }));
    return;
  }

  for (const cam of store.cameras) {
    grid.appendChild(renderTile(cam));
  }
}

function renderTile(cam) {
  const running = isActive(cam);
  const statusLabel = t(cam.status === 'offline' ? 'statusOffline'
    : hasCap('camera_control') && cam.status === 'idle' ? 'statusIdle'
    : hasCap('camera_control') ? 'statusRunning' : 'statusOnline');

  const thumb = el('img', {
    className: 'tile-thumb', alt: cam.name || cam.id,
    src: running ? '' : '',
  });
  if (running) attachThumb(thumb, cam.id);

  const actions = [el('button', {
    className: 'btn-small', textContent: t('openLive'),
    onclick: () => { setCameraId(cam.id); showView('preview'); },
  })];
  if (hasCap('camera_control')) {
    actions.push(el('button', {
      className: 'btn-small', textContent: running ? t('stopStream') : t('startStream'),
      onclick: () => toggleStream(cam),
    }));
  }
  if (hasCap('recording')) {
    actions.push(el('button', {
      className: 'btn-small', textContent: t('recordingStart'),
      onclick: () => toggleRecording(cam),
    }));
  }
  if (hasCap('camera_management')) {
    actions.push(el('button', {
      className: 'btn-small btn-danger', textContent: t('deleteCamera'),
      onclick: () => deleteCamera(cam),
    }));
  }

  return el('div', { className: 'tile' + (running ? '' : ' offline'), dataset: { camera: cam.id } }, [
    el('div', { className: 'tile-media' }, [
      thumb,
      el('span', { className: 'tile-badge ' + (running ? 'badge-online' : 'badge-offline'), textContent: statusLabel }),
    ]),
    el('div', { className: 'tile-body' }, [
      el('div', { className: 'tile-title', textContent: cam.name || cam.id }),
      el('div', { className: 'tile-meta mono', textContent: tileMeta(cam) }),
    ]),
    el('div', { className: 'tile-actions' }, actions),
  ]);
}

function tileMeta(cam) {
  const parts = [];
  if (cam.resolution) parts.push(cam.resolution);
  if (cam.fps) parts.push(cam.fps + 'fps');
  if (cam.camera_type) parts.push(cam.camera_type);
  return esc(parts.join(' \u00b7 '));
}

/// Live thumbnail: MJPEG when available, otherwise snapshot polling.
function attachThumb(img, id) {
  if (hasCap('mjpeg')) {
    img.src = `/api/cameras/${id}/live?_=${Date.now()}`;
    return;
  }
  const tick = () => { img.src = `/api/cameras/${id}/snapshot?_=${Date.now()}`; };
  tick();
  tileTimers.set(id, setInterval(tick, 5000));
}

async function toggleStream(cam) {
  const running = isActive(cam);
  const r = await api.post(`/api/cameras/${cam.id}/${running ? 'stop' : 'start'}`);
  if (r.ok) {
    toast(t(running ? 'streamStopped' : 'streamStarted'), 'success');
    await refreshCameras();
    renderCameras();
  } else {
    toast(r.message || t('fetchError'), 'error');
  }
}

async function toggleRecording(cam) {
  const r = await api.get(`/api/cameras/${cam.id}/recording`);
  const active = r.ok && r.data && r.data.active;
  const r2 = await api.post(`/api/cameras/${cam.id}/recording`, { active: !active });
  if (r2.ok) toast(t(active ? 'recordingStopped' : 'recordingStarted'), 'success');
  else toast(r2.message || t('fetchError'), 'error');
}

async function deleteCamera(cam) {
  if (!window.confirm(t('deleteConfirm', { name: cam.name || cam.id }))) return;
  const r = await api.del(`/api/cameras/${cam.id}`);
  if (r.ok) {
    await refreshCameras();
    renderCameras();
  } else {
    toast(r.message || t('fetchError'), 'error');
  }
}

export function initCameras() {
  const view = $('view-cameras');
  if (view) view.classList.toggle('hidden-cap', !hasCap('multi_camera'));
  const tab = document.querySelector('.nav-tab[data-view="cameras"]');
  if (tab) tab.classList.toggle('hidden', !hasCap('multi_camera'));
}
