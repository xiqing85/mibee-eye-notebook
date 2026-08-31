// Devices view (extension: devices): host video/audio enumeration and
// "use as camera" one-click flow.

import { api } from './api.js';
import { hasCap } from './store.js';
import { $, el, toast } from './ui.js';
import { t } from './i18n.js';
import { refreshCameras, renderCameras } from './cameras.js';

export async function renderDevices() {
  const view = $('view-devices');
  if (!view) return;
  const videoBox = $('video-devices');
  const audioBox = $('audio-devices');
  const capsBox = $('device-caps');

  const [videoR, audioR] = await Promise.all([
    api.get('/api/devices/video'),
    api.get('/api/devices/audio'),
  ]);

  videoBox.innerHTML = '';
  if (videoR.ok && (videoR.data || []).length > 0) {
    for (const dev of videoR.data) {
      videoBox.appendChild(el('div', { className: 'device-row' }, [
        el('div', { className: 'device-info' }, [
          el('span', { className: 'device-name', textContent: dev.index + '. ' + (dev.name || '?') }),
          (dev.formats || []).length > 0
            ? el('span', { className: 'device-meta mono', textContent: dev.formats.slice(0, 3).join(' ') })
            : el('span'),
        ]),
        hasCap('camera_management') ? el('button', {
          className: 'btn-small', textContent: t('useAsCamera'),
          onclick: () => addCamera(dev),
        }) : el('span'),
      ]));
    }
  } else {
    videoBox.appendChild(el('div', { className: 'empty-state', textContent: t('noDevices') }));
  }

  audioBox.innerHTML = '';
  if (audioR.ok && (audioR.data || []).length > 0) {
    for (const dev of audioR.data) {
      audioBox.appendChild(el('div', { className: 'device-row' }, [
        el('span', { className: 'device-name', textContent: dev.name || '?' }),
      ]));
    }
  } else {
    audioBox.appendChild(el('div', { className: 'empty-state', textContent: t('noDevices') }));
  }

  // Host capability summary (notebook-style probe), best-effort.
  capsBox.innerHTML = '';
  const capR = await api.get('/api/capabilities');
  const sys = capR.ok && capR.data && capR.data.system;
  if (sys) {
    const rows = [
      [t('cores'), sys.cpu_cores || (sys.cpu && sys.cpu.cores)],
      [t('memory'), sys.memory_mib ? sys.memory_mib + ' MB' : undefined],
      [t('encoder'), sys.recommended_encoder],
    ];
    for (const [label, val] of rows) {
      if (val === undefined || val === null) continue;
      capsBox.appendChild(el('div', { className: 'reading' }, [
        el('span', { className: 'reading-label', textContent: label }),
        el('span', { className: 'reading-val', textContent: String(val) }),
      ]));
    }
  }
}

async function addCamera(dev) {
  const r = await api.post('/api/cameras', {
    name: dev.name || ('Camera ' + dev.index),
    camera_type: 'usb',
    config: { device_index: dev.index },
  });
  if (r.ok) {
    toast(t('saved'), 'success');
    await refreshCameras();
    renderCameras();
  } else {
    toast(r.message || t('fetchError'), 'error');
  }
}

export function initDevices() {
  const view = $('view-devices');
  if (view) view.classList.toggle('hidden-cap', !hasCap('devices'));
  const tab = document.querySelector('.nav-tab[data-view="devices"]');
  if (tab) tab.classList.toggle('hidden', !hasCap('devices'));
}
