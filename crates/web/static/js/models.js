// AI model manager page (extensions: model_manager + cloud_ai, SPEC
// §4.9/§4.10). Every AI capability lists the models the device can
// actually run: download with live progress (SSE model_task), activate
// (restart-class capabilities ride the auto-restart flow; the detection
// capability hot-switches), delete installed files. The cloud card
// configures OpenRouter — the API key is write-only (GET returns only
// api_key_set), so the field stays blank unless the user types a new one.

import { api } from './api.js';
import { hasCap } from './store.js';
import { $, el, confirmDlg, toast } from './ui.js';
import { t } from './i18n.js';
import { beginDeviceRestart } from './restart.js';

let catalog = null;
let progressEls = new Map(); // "cap/model" → { bar, text, wrap }
let refreshTimer = null;

function fmtSize(bytes) {
  if (!bytes && bytes !== 0) return '';
  if (bytes >= 1024 * 1024 * 1024) return (bytes / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
  if (bytes >= 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
  return Math.max(1, Math.round(bytes / 1024)) + ' KB';
}

function capLabel(cap) {
  const key = 'capLabel_' + cap.id.replace(/\./g, '_');
  const s = t(key);
  return s === key ? (cap.label || cap.id) : s;
}

function runningTask(capId, modelId) {
  const tasks = (catalog && catalog.tasks) || [];
  return tasks.find((x) => x.capability === capId && x.model_id === modelId
    && (x.status === 'downloading' || x.status === 'verifying'));
}

export function updateModelsVisibility() {
  const show = hasCap('model_manager');
  document.querySelectorAll('.nav-tab[data-view="models"]').forEach((tab) => {
    tab.classList.toggle('hidden', !show);
  });
  const cloud = $('cloud-card');
  if (cloud) cloud.classList.toggle('hidden', !hasCap('cloud_ai'));
}

export function initModels() {
  const refresh = $('models-refresh');
  if (refresh) refresh.addEventListener('click', () => renderModels());
  const save = $('cloud-save');
  if (save) save.addEventListener('click', saveCloud);
  const test = $('cloud-test');
  if (test) test.addEventListener('click', testCloud);
  const clear = $('cloud-key-clear');
  if (clear) clear.addEventListener('click', clearCloudKey);
}

export async function renderModels() {
  const view = $('view-models');
  if (!view || !hasCap('model_manager')) return;
  const r = await api.get('/api/models');
  if (!r.ok) {
    $('models-caps').innerHTML = '';
    $('models-caps').appendChild(el('div', { className: 'record-empty', textContent: t('modelsLoadFailed') }));
    return;
  }
  catalog = r.data || { capabilities: [], tasks: [] };
  if (hasCap('cloud_ai')) await renderCloud();
  renderCapabilities();
}

// -- catalog cards ----------------------------------------------------------

function renderCapabilities() {
  progressEls = new Map();
  const wrap = $('models-caps');
  wrap.innerHTML = '';
  for (const cap of catalog.capabilities || []) {
    const card = el('div', { className: 'card', id: 'model-card-' + cap.id.replace(/\./g, '-') });
    const head = el('div', { className: 'view-head' }, [
      el('h3', { textContent: capLabel(cap) }),
      el('p', { className: 'view-desc' }, [
        el('span', {
          className: 'model-apply-badge ' + (cap.apply === 'immediate' ? 'badge-immediate' : 'badge-restart'),
          textContent: cap.apply === 'immediate' ? t('modelApplyImmediate') : t('modelApplyRestart'),
        }),
      ]),
    ]);
    card.appendChild(head);
    const list = el('div', { className: 'model-list' });
    for (const m of cap.models || []) list.appendChild(modelRow(cap, m));
    card.appendChild(list);
    wrap.appendChild(card);
  }
}

function modelRow(cap, m) {
  const key = cap.id + '/' + m.id;
  const task = runningTask(cap.id, m.id);
  const row = el('div', { className: 'model-row' + (m.active ? ' model-row-active' : '') });

  const badges = [];
  if (m.active) badges.push(el('span', { className: 'model-badge badge-active', textContent: t('modelActive') }));
  else if (m.installed) badges.push(el('span', { className: 'model-badge badge-installed', textContent: t('modelInstalled') }));
  if (!m.downloadable) badges.push(el('span', { className: 'model-badge badge-nodl', textContent: t('modelDownloadableFalse') }));

  const meta = [
    m.size_bytes ? fmtSize(m.size_bytes) : '',
    (m.languages || []).join('/'),
    m.license || '',
  ].filter(Boolean).join(' · ');
  const info = el('div', { className: 'model-info' }, [
    el('div', { className: 'model-name', textContent: m.name || m.id }, badges),
    meta ? el('div', { className: 'model-meta', textContent: meta }) : el('span'),
    m.notes ? el('div', { className: 'model-meta model-notes', textContent: m.notes }) : el('span'),
  ]);

  const barWrap = el('div', { className: 'model-progress hidden' });
  const bar = el('div', { className: 'model-progress-bar' });
  const barText = el('span', { className: 'model-progress-text mono' });
  barWrap.appendChild(bar);
  barWrap.appendChild(barText);
  info.appendChild(barWrap);
  row.appendChild(info);

  const actions = el('div', { className: 'model-actions' });
  if (!m.active && m.installed) {
    const btn = el('button', { type: 'button', className: 'btn-small', textContent: t('modelActivate') });
    btn.addEventListener('click', () => activateModel(cap, m));
    actions.appendChild(btn);
  }
  if (task) {
    barWrap.classList.remove('hidden');
    const cancel = el('button', { type: 'button', className: 'btn-small btn-danger', textContent: t('modelCancelTask') });
    cancel.addEventListener('click', async () => {
      const r = await api.post('/api/models/tasks/' + encodeURIComponent(task.task_id) + '/cancel', {});
      if (!r.ok) toast(r.message || t('error'), 'error');
      renderModels();
    });
    actions.appendChild(cancel);
    applyTaskToRow(key, task);
  } else if (m.downloadable) {
    const btn = el('button', {
      type: 'button',
      className: 'btn-small' + (m.installed ? '' : ' btn-primary'),
      textContent: m.installed ? t('modelRedownload') : t('modelDownload'),
    });
    btn.addEventListener('click', () => downloadModel(cap, m, btn));
    actions.appendChild(btn);
  }
  if (m.installed && !m.active) {
    const del = el('button', { type: 'button', className: 'btn-small btn-danger', textContent: t('modelDelete') });
    del.addEventListener('click', async () => {
      const ok = await confirmDlg({
        message: t('modelDeleteConfirm').replace('{name}', m.name || m.id),
        okText: t('confirm'),
        cancelText: t('cancel'),
        danger: true,
      });
      if (!ok) return;
      const r = await api.del('/api/models/' + encodeURIComponent(cap.id) + '/' + encodeURIComponent(m.id));
      if (!r.ok) toast(r.message || t('error'), 'error');
      renderModels();
    });
    actions.appendChild(del);
  }
  row.appendChild(actions);
  progressEls.set(key, { bar, text: barText, wrap: barWrap });
  return row;
}

async function downloadModel(cap, m, btn) {
  btn.disabled = true;
  const r = await api.post('/api/models/' + encodeURIComponent(cap.id) + '/' + encodeURIComponent(m.id) + '/download', {});
  btn.disabled = false;
  if (!r.ok) {
    if (r.error === 'conflict') {
      // Already installed → offer the force re-download.
      const ok = await confirmDlg({
        message: t('modelRedownloadConfirm').replace('{name}', m.name || m.id),
        okText: t('confirm'),
        cancelText: t('cancel'),
      });
      if (!ok) return;
      const f = await api.post('/api/models/' + encodeURIComponent(cap.id) + '/' + encodeURIComponent(m.id) + '/download', { force: true });
      if (!f.ok) { toast(f.message || t('error'), 'error'); return; }
    } else {
      toast(r.message || t('error'), 'error');
      return;
    }
  }
  toast(t('modelDownloading'), 'info');
  renderModels();
}

async function activateModel(cap, m) {
  if (cap.apply !== 'immediate') {
    const ok = await confirmDlg({
      message: t('modelActivateConfirm').replace('{name}', m.name || m.id),
      okText: t('confirm'),
      cancelText: t('cancel'),
    });
    if (!ok) return;
  }
  const r = await api.post('/api/models/' + encodeURIComponent(cap.id) + '/' + encodeURIComponent(m.id) + '/activate', {});
  if (!r.ok) {
    toast(r.message || t('error'), 'error');
    return;
  }
  if ((r.data && r.data.applied) === 'restart' && hasCap('restart')) {
    toast(t('modelActivated'), 'success');
    beginDeviceRestart();
  } else {
    toast(t('modelActivated'), 'success');
    renderModels();
  }
}

// -- SSE model_task hook (progress without polling) -------------------------

export function handleModelTask(p) {
  if (!p || !catalog) return;
  const key = p.capability + '/' + p.model_id;
  // Keep the task list in sync so runningTask() sees the live state.
  const tasks = catalog.tasks || (catalog.tasks = []);
  const idx = tasks.findIndex((x) => x.task_id === p.task_id);
  if (idx >= 0) tasks[idx] = Object.assign(tasks[idx], p);
  else tasks.push(p);
  if (p.status === 'downloading' || p.status === 'verifying') {
    applyTaskToRow(key, p);
    return;
  }
  // Terminal state: refresh the catalog once (installed badges appear).
  if (p.status === 'done') toast(t('modelTaskDone').replace('{name}', p.model_name || p.model_id), 'success');
  if (p.status === 'failed') toast(t('modelTaskFailed').replace('{name}', p.model_name || p.model_id) + (p.error ? ': ' + p.error : ''), 'error');
  if ($('view-models').classList.contains('active')) {
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(renderModels, 400);
  }
}

function applyTaskToRow(key, task) {
  const slot = progressEls.get(key);
  if (!slot) return;
  slot.wrap.classList.remove('hidden');
  const pct = Math.round((task.progress || 0) * 100);
  slot.bar.style.width = pct + '%';
  slot.text.textContent = (task.status === 'verifying' ? t('modelVerifying') : pct + '%')
    + ' · ' + fmtSize(task.downloaded_bytes) + (task.total_bytes ? ' / ' + fmtSize(task.total_bytes) : '');
}

// -- cloud card (SPEC §4.10) ------------------------------------------------

async function renderCloud() {
  const r = await api.get('/api/cloud');
  if (!r.ok) return;
  const c = r.data || {};
  cloudState = c;
  $('cloud-provider').value = c.provider || 'off';
  $('cloud-api-key').value = '';
  $('cloud-chat-model').value = c.chat_model || '';
  $('cloud-vision-model').value = c.vision_model || '';
  $('cloud-fallback').checked = c.fallback_local !== false;
  $('cloud-key-state').textContent = c.api_key_set ? t('cloudApiKeySet') : t('cloudApiKeyUnset');
  const clearBtn = $('cloud-key-clear');
  if (clearBtn) clearBtn.classList.toggle('hidden', !c.api_key_set);
  const cs = $('cloud-chat-suggest');
  const vs = $('cloud-vision-suggest');
  cs.innerHTML = '';
  vs.innerHTML = '';
  for (const id of ((c.suggest || {}).chat) || []) cs.appendChild(el('option', { value: id }));
  for (const id of ((c.suggest || {}).vision) || []) vs.appendChild(el('option', { value: id }));
}

let cloudState = null;

async function saveCloud() {
  const body = {
    provider: $('cloud-provider').value,
    chat_model: $('cloud-chat-model').value.trim(),
    vision_model: $('cloud-vision-model').value.trim(),
    fallback_local: $('cloud-fallback').checked,
  };
  const key = $('cloud-api-key').value;
  if (key) body.api_key = key; // absent = keep the stored key untouched
  const r = await api.put('/api/cloud', body);
  if (!r.ok) {
    toast(r.message || t('error'), 'error');
    return;
  }
  toast(t('cloudSaved'), 'success');
  renderCloud();
}

async function clearCloudKey() {
  const ok = await confirmDlg({
    message: t('cloudKeyClearConfirm'),
    okText: t('confirm'),
    cancelText: t('cancel'),
    danger: true,
  });
  if (!ok) return;
  const r = await api.put('/api/cloud', { api_key: '' });
  if (!r.ok) {
    toast(r.message || t('error'), 'error');
    return;
  }
  toast(t('cloudKeyCleared'), 'success');
  renderCloud();
}

async function testCloud() {
  const btn = $('cloud-test');
  btn.disabled = true;
  const saved = await api.put('/api/cloud', {
    provider: $('cloud-provider').value,
    chat_model: $('cloud-chat-model').value.trim(),
    vision_model: $('cloud-vision-model').value.trim(),
    fallback_local: $('cloud-fallback').checked,
    ...(($('cloud-api-key').value) ? { api_key: $('cloud-api-key').value } : {}),
  });
  if (!saved.ok) {
    btn.disabled = false;
    toast(saved.message || t('error'), 'error');
    return;
  }
  const r = await api.post('/api/cloud/test', {});
  btn.disabled = false;
  if (r.ok && r.data && r.data.ok) {
    toast(t('cloudTestOk').replace('{ms}', String(r.data.latency_ms)).replace('{model}', r.data.model || ''), 'success');
  } else {
    toast(t('cloudTestFail') + (r.message ? ': ' + r.message : ''), 'error');
  }
  renderCloud();
}
