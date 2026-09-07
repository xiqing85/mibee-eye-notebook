// AI model picker (extension: ai + ai_models, SPEC v1 §4.6).
//
// Capability-gated: only devices that advertise `ai_models` show the
// selector. Switching POSTs the activate endpoint (applied immediately,
// rollback server-side); `ai_model_changed` SSE events keep every open
// client in sync without a reload.

import { api } from './api.js';
import { store, hasCap } from './store.js';
import { $, toast } from './ui.js';
import { t } from './i18n.js';

export function aiModelsEnabled() {
  return hasCap('ai') && hasCap('ai_models');
}

async function refreshModels() {
  if (!aiModelsEnabled()) return;
  const data = await api.get('/api/ai/models').catch(() => null);
  if (!data) return;
  renderModelSelect(data);
}

function wireUpload(store2) {
  const field = $('ai-upload-field');
  const btn = $('ai-upload-btn');
  const file = $('ai-upload-file');
  if (!field || !btn || !file) return;
  const allowed = !!(store2.aiModels && store2.aiModels.upload && store2.aiModels.upload.allowed);
  field.classList.toggle('hidden', !allowed);
  btn.classList.toggle('hidden', !allowed);
  if (btn.dataset.wired) return;
  btn.dataset.wired = '1';
  btn.addEventListener('click', () => file.click());
  file.addEventListener('change', async () => {
    const f = file.files && file.files[0];
    if (!f) return;
    const id = f.name.replace(/\.onnx$/i, '').toLowerCase()
      .replace(/[^a-z0-9-]+/g, '-').replace(/^-+/, '');
    const family = $('ai-upload-family').value || 'nanodet';
    const fd = new FormData();
    fd.append('family', family);
    fd.append('file', f, f.name);
    try {
      await api.postForm(`/api/ai/models/${encodeURIComponent(id)}`, fd);
      toast(t('aiModelSwitched', { model: id }), 'success');
      await refreshModels();
    } catch (e) {
      toast(t('aiUploadFail') + (e && e.message ? ': ' + e.message : ''), 'error');
    }
    file.value = '';
  });
}

export function renderModelSelect(data) {
  const field = $('ai-model-field');
  const select = $('ai-model-select');
  if (!field || !select) return;
  field.classList.toggle('hidden', !aiModelsEnabled());
  if (!aiModelsEnabled()) return;

  store.aiModels = data;
  wireUpload(store);
  // Upload controls (SPEC §4.6 ai_upload; hidden unless the device allows).
  const upField = $('ai-upload-field');
  if (upField) upField.classList.toggle('hidden', !(data.upload && data.upload.allowed));
  const active = data.active || '';
  select.innerHTML = '';
  for (const m of (data.models || [])) {
    const opt = document.createElement('option');
    opt.value = m.id;
    opt.textContent = m.id + (m.input ? ' · ' + m.input : '');
    opt.selected = m.id === active;
    if (!m.available) {
      opt.disabled = true;
      opt.title = t('aiModelUnavailable');
    }
    if (m.id === 'custom') opt.title = t('aiModelCustom');
    select.appendChild(opt);
  }
  if (!select.querySelector('option[selected]') && active) {
    // Active model missing from the list (e.g. custom override resolved at
    // boot) — show it as a locked extra entry so the UI never lies.
    const opt = document.createElement('option');
    opt.value = active;
    opt.textContent = active;
    opt.selected = true;
    opt.disabled = true;
    select.appendChild(opt);
  }
}

async function activateModel(id) {
  try {
    const data = await api.post(`/api/ai/models/${encodeURIComponent(id)}/activate`);
    toast(t('aiModelSwitched', { model: data.active || id }), 'success');
    // SSE ai_model_changed refreshes the select for everyone; refresh now
    // anyway so single-client setups update even without a stream.
    await refreshModels();
  } catch (e) {
    toast(t('aiModelSwitchFail') + (e && e.message ? ': ' + e.message : ''), 'error');
    await refreshModels();
  }
}

export function handleModelChanged(payload) {
  if (!payload || !payload.model) return;
  toast(t('aiModelSwitched', { model: payload.model }), 'info');
  refreshModels();
}

async function uploadModel(file) {
  const family = $('ai-upload-family') ? $('ai-upload-family').value : 'nanodet';
  const id = (file.name || 'uploaded-model').replace(/\.onnx$/, '')
    .toLowerCase().replace(/[^a-z0-9-]+/g, '-').replace(/^-+|-+$/g, '')
    .slice(0, 64) || 'uploaded-model';
  const form = new FormData();
  form.append('family', family);
  form.append('file', file);
  try {
    await api.postForm(`/api/ai/models/${encodeURIComponent(id)}`, form);
    toast(t('aiModelUploaded', { id }), 'success');
    await refreshModels();
  } catch (e) {
    toast(t('aiModelUploadFail') + (e && e.message ? ': ' + e.message : ''), 'error');
  }
}

export function deleteModel(id) {
  return api.del(`/api/ai/models/${encodeURIComponent(id)}`)
    .then(refreshModels)
    .catch((e) => toast(t('aiModelUploadFail') + (e && e.message ? ': ' + e.message : ''), 'error'));
}

export function initAi() {
  const select = $('ai-model-select');
  if (!select) return;
  select.addEventListener('change', () => {
    if (select.value) activateModel(select.value);
  });
  const upBtn = $('ai-upload-btn');
  if (upBtn) {
    upBtn.addEventListener('click', () => {
      const fi = $('ai-upload-file');
      if (fi) fi.click();
    });
  }
  const fileInput = $('ai-upload-file');
  if (fileInput) {
    fileInput.addEventListener('change', () => {
      const f = fileInput.files && fileInput.files[0];
      if (f) uploadModel(f);
      fileInput.value = '';
    });
  }
  refreshModels();
}
