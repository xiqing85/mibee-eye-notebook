// Imaging panel (extension: imaging). Sliders / enums / toggles driven by
// ONVIF-style PascalCase params; writes are debounced and immediately applied.

import { api } from './api.js';
import { cameraId, hasCap } from './store.js';
import { $, el, toast, confirmDlg } from './ui.js';
import { t } from './i18n.js';
import { beginDeviceRestart, restartPending } from './restart.js';

const SLIDERS = [
  { name: 'Brightness', min: -1, max: 1, step: 0.01 },
  { name: 'Contrast', min: 0, max: 2, step: 0.01 },
  { name: 'Saturation', min: 0, max: 2, step: 0.01 },
  { name: 'Sharpness', min: 0, max: 8, step: 0.1 },
];
const AWB_ENUMS = ['auto', 'incandescent', 'fluorescent', 'warm_fluorescent', 'daylight', 'cloudy', 'shade'];
const EXPOSURE_ENUMS = ['normal', 'sports', 'night', 'backlight', 'spotlight', 'snow', 'beach', 'verylong', 'fixedfps', 'antishake', 'fireworks'];
const postTimers = {};

function postParam(name, value) {
  clearTimeout(postTimers[name]);
  postTimers[name] = setTimeout(async () => {
    const r = await api.post(`/api/cameras/${cameraId()}/imaging/param`, { name, value });
    if (!r.ok) { toast(t('paramError', { name }), 'error'); return; }
    if (r.data && r.data.applied === 'restart') beginDeviceRestart();
  }, 150);
}

async function resetDefaults() {
  const ok = await confirmDlg({
    message: t('imagingResetConfirm'),
    okText: t('imagingReset'),
    cancelText: t('cancel'),
    danger: true,
  });
  if (!ok) return;
  const defaults = {
    Brightness: 0, Contrast: 1, Saturation: 1, Sharpness: 1,
    AWBMode: 'auto', ExposureMode: 'normal', HFlip: false, VFlip: false,
  };
  for (const [name, value] of Object.entries(defaults)) {
    try {
      const r = await api.post(`/api/cameras/${cameraId()}/imaging/param`, { name, value });
      if (r.ok && r.data && r.data.applied === 'restart') beginDeviceRestart();
    } catch { /* device may already be restarting from a flip reset */ }
  }
  if (!restartPending()) loadImaging();
}

export function initImaging() {
  const section = $('imaging-section');
  if (!section || !hasCap('imaging')) {
    if (section) section.classList.add('hidden');
    return;
  }
  section.classList.remove('hidden');
  const resetBtn = $('imaging-reset');
  if (resetBtn && !resetBtn._wired) {
    resetBtn._wired = true;
    resetBtn.addEventListener('click', resetDefaults);
  }
  loadImaging();
}

async function loadImaging() {
  const container = $('imaging-controls');
  if (!container) return;
  container.innerHTML = '<div class="loading-state"><div class="spinner"></div><span>' + t('loading') + '</span></div>';
  const id = cameraId();
  const [paramsR, optionsR] = await Promise.all([
    api.get(`/api/cameras/${id}/imaging/params`),
    api.get(`/api/cameras/${id}/imaging/options`),
  ]);
  if (!paramsR.ok) {
    container.innerHTML = '<div class="msg-error">' + t('imagingLoad') + '</div>';
    return;
  }
  renderImaging(paramsR.data || {}, optionsR.data || {});
}

function renderImaging(params, options) {
  const container = $('imaging-controls');
  if (!container) return;
  container.className = 'imaging-controls-grid';
  container.innerHTML = '';

  for (const cfg of SLIDERS) {
    const current = params[cfg.name] !== undefined ? Number(params[cfg.name]) : 0;
    const range = options[cfg.name] || {};
    const min = range.min !== undefined ? range.min : cfg.min;
    const max = range.max !== undefined ? range.max : cfg.max;
    const step = range.step !== undefined ? range.step : cfg.step;

    const value = el('span', { className: 'imaging-value mono', textContent: fmt(current, step) });
    const slider = el('input', {
      className: 'imaging-slider', type: 'range', id: 'imaging-' + cfg.name.toLowerCase(),
      min: String(min), max: String(max), step: String(step), value: String(current),
      'data-param': cfg.name,
    });
    slider.addEventListener('input', () => {
      value.textContent = fmt(Number(slider.value), step);
      updateFill(slider);
    });
    slider.addEventListener('change', () => postParam(cfg.name, Number(slider.value)));
    requestAnimationFrame(() => updateFill(slider));

    container.appendChild(el('div', { className: 'imaging-control' }, [
      el('div', { className: 'imaging-header' }, [
        el('label', { className: 'imaging-label', for: slider.id, textContent: t('imaging.' + cfg.name) }),
        value,
      ]),
      slider,
      el('div', { className: 'imaging-range-labels' }, [
        el('span', { textContent: String(min) }), el('span', { textContent: String(max) }),
      ]),
    ]));
  }

  container.appendChild(buildSelect('AWBMode', params.AWBMode || 'auto',
    (options.AWBMode && options.AWBMode.enums) || AWB_ENUMS));
  container.appendChild(buildSelect('ExposureMode', params.ExposureMode || 'normal',
    (options.ExposureMode && options.ExposureMode.enums) || EXPOSURE_ENUMS));

  for (const name of ['HFlip', 'VFlip']) {
    const input = el('input', { type: 'checkbox', 'data-param': name });
    if (params[name]) input.checked = true;
    input.addEventListener('change', () => postParam(name, input.checked));
    container.appendChild(el('div', { className: 'imaging-control imaging-bool' }, [
      el('span', { className: 'imaging-label', textContent: t('imaging.' + name) }),
      el('label', { className: 'switch' }, [input, el('span', { className: 'switch-slider' })]),
    ]));
  }
}

function buildSelect(name, current, enums) {
  const sel = el('select', { className: 'imaging-select', 'data-param': name });
  for (const opt of enums) {
    const o = el('option', { value: opt, textContent: opt });
    if (opt === current) o.selected = true;
    sel.appendChild(o);
  }
  sel.addEventListener('change', () => postParam(name, sel.value));
  return el('div', { className: 'imaging-control' }, [
    el('label', { className: 'imaging-label', textContent: t('imaging.' + name), for: sel.dataset.param }),
    sel,
  ]);
}

function updateFill(slider) {
  const min = Number(slider.min) || 0;
  const max = Number(slider.max) || 1;
  const pct = ((Number(slider.value) - min) / (max - min)) * 100;
  slider.style.setProperty('--val', pct + '%');
}

function fmt(v, step) {
  if (step >= 1) return String(Math.round(v));
  if (step >= 0.1) return v.toFixed(1);
  return v.toFixed(2);
}

/// SSE param_changed: refresh the panel unless the change came from us.
export function handleParamChanged(payload) {
  if (!payload || !payload.name) return;
  const node = document.querySelector(`#imaging-controls [data-param="${payload.name}"]`);
  if (!node) return;
  if (node.type === 'checkbox') node.checked = !!payload.value;
  else node.value = payload.value;
  node.dispatchEvent(new Event('input'));
}
