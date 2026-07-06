// Modal dialogs: Add/Edit Camera + Delete Confirm.
import { h } from 'preact';
import { useState, useEffect } from 'preact/hooks';
import { t } from '../app.jsx';
import { api } from '../api.js';
import { showToast } from '../store.js';

export function CameraModal({ initial, onClose, onSaved }) {
  const isEdit = !!initial;
  const [name, setName] = useState(initial?.name || '');
  const [type, setType] = useState(initial?.camera_type || '');
  const [config, setConfig] = useState(() => {
    if (!initial?.config) return '';
    return typeof initial.config === 'string' ? initial.config : JSON.stringify(initial.config, null, 2);
  });
  const [deviceIndex, setDeviceIndex] = useState(initial?.config?.device_index ?? '');
  const [devices, setDevices] = useState(null);
  const [showDeviceSelect, setShowDeviceSelect] = useState(initial?.camera_type === 'usb');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  useEffect(() => {
    if (showDeviceSelect && devices === null) {
      api.get('/api/devices/video').then((r) => { if (r.ok) setDevices(r.data || []); });
    }
  }, [showDeviceSelect]);

  useEffect(() => {
    setShowDeviceSelect(type === 'usb');
  }, [type]);

  async function save() {
    setError('');
    if (!name) { setError(t('camera.error_name')); return; }
    if (!type) { setError(t('camera.error_type')); return; }
    let cfg;
    if (type === 'usb' && deviceIndex !== '') {
      cfg = { device_index: Number(deviceIndex) };
    } else {
      if (config.trim()) {
        try { cfg = JSON.parse(config); } catch { setError(t('camera.error_json')); return; }
      } else {
        cfg = {};
      }
    }
    setBusy(true);
    const body = { name, camera_type: type, config: cfg };
    const res = isEdit
      ? await api.put(`/api/cameras/${initial.id}`, body)
      : await api.post('/api/cameras', body);
    setBusy(false);
    if (res.ok) {
      showToast(isEdit ? t('camera.updated') : t('camera.added'), 'success');
      onSaved();
      onClose();
    } else {
      setError(res.data?.message || t('error.failed'));
    }
  }

  return (
    <div class="modal-overlay" role="dialog" aria-modal="true" onClick={onClose}>
      <div class="modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-head">
          <h2>{isEdit ? t('camera.edit') : t('camera.add')}</h2>
          <button class="modal-close" onClick={onClose} aria-label={t('common.close')}>×</button>
        </div>
        <div class="modal-body">
          {error && <div class="alert alert-error">{error}</div>}
          <div class="form-group">
            <label>{t('camera.name')}</label>
            <input value={name} onInput={(e) => setName(e.target.value)} placeholder={t('camera.name_ph')} />
          </div>
          <div class="form-group">
            <label>{t('camera.type')}</label>
            <select value={type} onChange={(e) => setType(e.target.value)} disabled={isEdit}>
              <option value="">{t('camera.select_type')}</option>
              <option value="usb">{t('camera.type_usb')}</option>
              <option value="rtsp">{t('camera.type_rtsp')}</option>
              <option value="onvif">{t('camera.type_onvif')}</option>
              <option value="gb28181">{t('camera.type_gb28181')}</option>
              <option value="rtmp">{t('camera.type_rtmp')}</option>
            </select>
          </div>
          {showDeviceSelect && (
            <div class="form-group">
              <label>{t('camera.device_label')}</label>
              <select value={deviceIndex} onChange={(e) => setDeviceIndex(e.target.value)}>
                <option value="">{t('camera.select_device')}</option>
                {(devices || []).map((d) => (
                  <option value={d.index}>[{d.index}] {d.name}</option>
                ))}
              </select>
            </div>
          )}
          {!showDeviceSelect && (
            <div class="form-group">
              <label>{t('camera.config')}</label>
              <textarea rows="3" value={config} onInput={(e) => setConfig(e.target.value)} placeholder={t('camera.config_ph')} />
              <span class="hint">{t('camera.config_hint')}</span>
            </div>
          )}
        </div>
        <div class="modal-foot">
          <button class="btn" onClick={onClose}>{t('common.cancel')}</button>
          <button class="btn btn-primary" onClick={save} disabled={busy}>
            {busy ? t('camera.saving') : (isEdit ? t('common.save') : t('camera.add'))}
          </button>
        </div>
      </div>
    </div>
  );
}

export function ConfirmDialog({ title, message, confirmLabel, onConfirm, onClose }) {
  return (
    <div class="modal-overlay" role="dialog" aria-modal="true" onClick={onClose}>
      <div class="modal modal-sm" onClick={(e) => e.stopPropagation()}>
        <div class="modal-confirm">
          <div class="warn-icon">⚠</div>
          <h3>{title}</h3>
          <p class="text-muted">{message}</p>
        </div>
        <div class="modal-foot center">
          <button class="btn" onClick={onClose}>{t('common.cancel')}</button>
          <button class="btn btn-danger" onClick={onConfirm}>{confirmLabel}</button>
        </div>
      </div>
    </div>
  );
}
