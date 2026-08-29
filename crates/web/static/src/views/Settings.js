// Settings view with four tabs: Server / Recording / Protocols / Security.
//
// Server/Recording tabs read+write the generic /api/settings store.
// Protocols tab hot-toggles ONVIF/GB28181/RTMP via their dedicated endpoints.
// Security tab changes the admin password.
import { h } from 'preact';
import { useState, useEffect } from 'preact/hooks';
import { t } from '../app.jsx';
import { api } from '../api.js';
import { showToast } from '../store.js';

const TABS = ['server', 'recording', 'protocols', 'security'];

export function Settings() {
  const [tab, setTab] = useState('server');
  const [settings, setSettings] = useState(null);
  const [protocols, setProtocols] = useState({ onvif: null, gb28181: null, rtmp: null });

  useEffect(() => {
    let alive = true;
    (async () => {
      const [s, o, g, r] = await Promise.all([
        api.get('/api/settings'),
        api.get('/api/protocols/onvif'),
        api.get('/api/protocols/gb28181'),
        api.get('/api/protocols/rtmp'),
      ]);
      if (!alive) return;
      if (s.ok) setSettings(s.data);
      if (o.ok) setProtocols((p) => ({ ...p, onvif: o.data }));
      if (g.ok) setProtocols((p) => ({ ...p, gb28181: g.data }));
      if (r.ok) setProtocols((p) => ({ ...p, rtmp: r.data }));
    })();
    return () => { alive = false; };
  }, []);

  if (!settings) return <div class="loading"><div class="spinner" /><span>{t('settings.loading')}</span></div>;

  return (
    <section class="page">
      <div class="page-head">
        <div><h1>{t('settings.title')}</h1><p class="text-muted">{t('settings.subtitle')}</p></div>
      </div>
      <div role="tablist" class="tabs">
        {TABS.map((tb) => (
          <button key={tb} role="tab" class={tab === tb ? 'tab active' : 'tab'}
                  aria-selected={tab === tb} onClick={() => setTab(tb)}>
            {t('settings.tab_' + tb)}
          </button>
        ))}
      </div>

      {tab === 'server' && <ServerTab settings={settings} setSettings={setSettings} />}
      {tab === 'recording' && <RecordingTab settings={settings} setSettings={setSettings} />}
      {tab === 'protocols' && <ProtocolsTab protocols={protocols} setProtocols={setProtocols} />}
      {tab === 'security' && <SecurityTab />}
    </section>
  );
}

function ServerTab({ settings, setSettings }) {
  const set = (k, v) => setSettings({ ...settings, [k]: v });
  async function save() {
    const res = await api.put('/api/settings', { settings });
    showToast(res.ok ? t('settings.saved') : t('settings.load_failed'), res.ok ? 'success' : 'error');
  }
  return (
    <div class="card">
      <h3>{t('settings.server')}</h3>
      <div class="form-group"><label>{t('settings.web_port')}</label>
        <input type="number" value={settings['web.port'] || ''} onInput={(e) => set('web.port', e.target.value)} placeholder={t('settings.web_port_ph')} /></div>
      <div class="form-group"><label>{t('settings.rtsp_port')}</label>
        <input type="number" value={settings['rtsp.port'] || ''} onInput={(e) => set('rtsp.port', e.target.value)} placeholder={t('settings.rtsp_port_ph')} /></div>
      <button class="btn btn-primary" onClick={save}>{t('settings.save')}</button>
    </div>
  );
}

function RecordingTab({ settings, setSettings }) {
  const set = (k, v) => setSettings({ ...settings, [k]: v });
  async function save() {
    const res = await api.put('/api/settings', { settings });
    showToast(res.ok ? t('settings.saved') : t('settings.load_failed'), res.ok ? 'success' : 'error');
  }
  return (
    <div class="card">
      <h3>{t('settings.recording')}</h3>
      <div class="form-group"><label>{t('settings.recordings_path')}</label>
        <input type="text" value={settings['recording.path'] || ''} onInput={(e) => set('recording.path', e.target.value)} placeholder={t('settings.recordings_path_ph')} /></div>
      <div class="form-group"><label>{t('settings.max_segment_duration')}</label>
        <input type="number" value={settings['recording.segment_duration_secs'] || ''} onInput={(e) => set('recording.segment_duration_secs', e.target.value)} placeholder={t('settings.max_segment_ph')} /></div>
      <button class="btn btn-primary" onClick={save}>{t('settings.save')}</button>
    </div>
  );
}

function ProtocolsTab({ protocols, setProtocols }) {
  async function toggle(name, enabled) {
    const res = await api.put(`/api/protocols/${name}`, { enabled });
    if (res.ok) {
      setProtocols({ ...protocols, [name]: { ...protocols[name], enabled } });
      showToast(t(enabled ? 'protocols.toggle_on' : 'protocols.toggle_off', { name }), 'info');
    }
  }
  async function saveFields(name) {
    const res = await api.put(`/api/protocols/${name}`, protocols[name]);
    showToast(res.ok ? t('protocol.saved', { name }) : t('error.failed'), res.ok ? 'success' : 'error');
  }
  const upd = (name, k, v) => setProtocols({ ...protocols, [name]: { ...protocols[name], [k]: v } });

  return (
    <div>
      <ProtocolCard name="onvif" label={t('protocol.onvif')} cfg={protocols.onvif}
        onToggle={toggle} onSave={saveFields} onUpdate={upd} fields={[
          { key: 'port', label: t('protocol.port'), type: 'number', ph: t('protocol.port_ph') },
          { key: 'device_name', label: t('protocol.device_name'), ph: t('protocol.device_name_ph') },
        ]} />
      <ProtocolCard name="gb28181" label={t('protocol.gb28181')} cfg={protocols.gb28181}
        onToggle={toggle} onSave={saveFields} onUpdate={upd} fields={[
          { key: 'device_id', label: t('protocol.device_id'), ph: t('protocol.device_id_ph') },
          { key: 'channel_id', label: t('protocol.channel_id'), ph: t('protocol.channel_id_ph') },
          { key: 'platform_sip_address', label: t('protocol.server_ip'), ph: t('protocol.server_ip_ph') },
          { key: 'platform_sip_port', label: t('protocol.server_port'), type: 'number', ph: t('protocol.server_port_ph') },
          { key: 'username', label: t('protocol.username'), ph: t('protocol.username_ph') },
          { key: 'password', label: t('protocol.password'), type: 'password', ph: t('protocol.password_ph') },
        ]} />
      <ProtocolCard name="rtmp" label={t('protocol.rtmp')} cfg={protocols.rtmp}
        onToggle={toggle} onSave={saveFields} onUpdate={upd} fields={[
          { key: 'push_url', label: t('protocol.url'), ph: t('protocol.url_ph') },
        ]} />
    </div>
  );
}

function ProtocolCard({ name, label, cfg, onToggle, onSave, onUpdate, fields }) {
  const [open, setOpen] = useState(true);
  if (!cfg) return null;
  return (
    <div class="card protocol-card">
      <div class="protocol-head" onClick={() => setOpen(!open)}>
        <label class="switch" onClick={(e) => e.stopPropagation()}>
          <input type="checkbox" checked={cfg.enabled || false} onChange={(e) => onToggle(name, e.target.checked)} />
          <span class="track" /><span class="thumb" />
        </label>
        <h3>{label}</h3>
        <span class="chevron">{open ? '▾' : '▸'}</span>
      </div>
      {open && (
        <div class="protocol-body">
          {fields.map((f) => (
            <div class="form-group" key={f.key}>
              <label for={`${name}-${f.key}`}>{f.label}</label>
              <input id={`${name}-${f.key}`} type={f.type || 'text'} value={cfg[f.key] || ''} placeholder={f.ph}
                     onInput={(e) => onUpdate(name, f.key, e.target.value)} />
            </div>
          ))}
          <button class="btn btn-primary" onClick={() => onSave(name)}>{t('protocol.save', { name })}</button>
        </div>
      )}
    </div>
  );
}

function SecurityTab() {
  const [cur, setCur] = useState('');
  const [npw, setNpw] = useState('');
  const [conf, setConf] = useState('');
  const [busy, setBusy] = useState(false);

  async function change() {
    if (!cur || !npw || !conf) { showToast(t('settings.fields_required'), 'error'); return; }
    if (npw !== conf) { showToast(t('settings.password_mismatch'), 'error'); return; }
    if (npw.length < 8) { showToast(t('settings.password_length'), 'error'); return; }
    setBusy(true);
    const res = await api.post('/api/auth/reset', { old_password: cur, new_password: npw });
    setBusy(false);
    if (res.ok) {
      showToast(t('settings.password_changed'), 'success');
      // Server invalidated sessions; force re-login.
      setTimeout(() => { window.location.hash = '#login'; location.reload(); }, 1500);
    } else {
      showToast(t('error.failed'), 'error');
    }
  }
  return (
    <div class="card">
      <h3>{t('settings.change_password')}</h3>
      <div class="form-group"><label>{t('settings.current_password')}</label>
        <input type="password" value={cur} onInput={(e) => setCur(e.target.value)} autocomplete="current-password" /></div>
      <div class="form-group"><label>{t('settings.new_password')}</label>
        <input type="password" value={npw} onInput={(e) => setNpw(e.target.value)} autocomplete="new-password" /></div>
      <div class="form-group"><label>{t('settings.confirm_new_password')}</label>
        <input type="password" value={conf} onInput={(e) => setConf(e.target.value)} autocomplete="new-password" /></div>
      <button class="btn btn-primary" onClick={change} disabled={busy}>{t('settings.change_password_btn')}</button>
    </div>
  );
}
