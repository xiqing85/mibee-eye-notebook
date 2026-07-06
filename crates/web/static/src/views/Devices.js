// Devices view: video/audio device enumeration + system capability panel.
//
// The capability panel surfaces the /api/capabilities data (probed CPU/GPU/
// encoder backends + recommended profiles) so the user can see what the host
// can do. Each video device links to its structured formats
// (/api/devices/video/{index}/formats).
import { h } from 'preact';
import { useState, useEffect } from 'preact/hooks';
import { t } from '../app.jsx';
import { api } from '../api.js';
import { capabilities, showToast } from '../store.js';

export function Devices({ onUseAsCamera }) {
  const [videoDevs, setVideoDevs] = useState(null);
  const [audioDevs, setAudioDevs] = useState(null);
  const [formatsByIndex, setFormatsByIndex] = useState({});
  const [caps, setCaps] = useState(capabilities.value);

  useEffect(() => {
    let alive = true;
    (async () => {
      const [v, a, c] = await Promise.all([
        api.get('/api/devices/video'),
        api.get('/api/devices/audio'),
        api.get('/api/capabilities'),
      ]);
      if (!alive) return;
      if (v.ok) setVideoDevs(v.data || []);
      if (a.ok) setAudioDevs(a.data || []);
      if (c.ok) { setCaps(c.data); capabilities.value = c.data; }
    })();
    return () => { alive = false; };
  }, []);

  async function loadFormats(index) {
    if (formatsByIndex[index]) {
      // Toggle off.
      const next = { ...formatsByIndex };
      delete next[index];
      setFormatsByIndex(next);
      return;
    }
    const res = await api.get(`/api/devices/video/${index}/formats`);
    if (res.ok) {
      setFormatsByIndex({ ...formatsByIndex, [index]: res.data });
    }
  }

  return (
    <section class="page">
      <div class="page-head">
        <div><h1>{t('devices.title')}</h1><p class="text-muted">{t('devices.subtitle')}</p></div>
      </div>

      {caps && <CapabilityPanel caps={caps} />}

      <h2>{t('devices.video')}</h2>
      {videoDevs === null ? <div class="loading"><div class="spinner" /></div> :
       videoDevs.length === 0 ? <div class="empty"><p>{t('devices.no_video')}</p></div> :
       videoDevs.map((d) => (
        <div class="card device-card" key={d.index}>
          <div class="device-head">
            <h3>[{d.index}] {d.name}</h3>
            <div class="device-actions">
              <button class="btn btn-sm" onClick={() => loadFormats(d.index)}>
                {formatsByIndex[d.index] ? t('common.close') : t('devices.formats')}
              </button>
              <button class="btn btn-sm btn-primary" onClick={() => onUseAsCamera(d)}>
                {t('camera.use_as_camera')}
              </button>
            </div>
          </div>
          {formatsByIndex[d.index] && (
            <FormatTable formats={formatsByIndex[d.index]} />
          )}
        </div>
      ))}

      <h2>{t('devices.audio')}</h2>
      {audioDevs === null ? <div class="loading"><div class="spinner" /></div> :
       audioDevs.length === 0 ? <div class="empty"><p>{t('devices.no_audio')}</p></div> :
       audioDevs.map((d, i) => (
        <div class="card device-card" key={i}>
          <h3>{d.name}</h3>
          {d.supported_configs && d.supported_configs.map((c, j) => (
            <div class="text-muted" key={j}>
              {c.channels}ch · {c.min_sample_rate}-{c.max_sample_rate}Hz · {c.sample_format}
            </div>
          ))}
        </div>
      ))}
    </section>
  );
}

function FormatTable({ formats }) {
  return (
    <table class="data-table compact">
      <thead><tr><th>Width</th><th>Height</th><th>Format</th><th>FPS</th></tr></thead>
      <tbody>
        {formats.map((f, i) => (
          <tr key={i}><td>{f.width}</td><td>{f.height}</td><td>{f.format}</td><td>{f.fps}</td></tr>
        ))}
      </tbody>
    </table>
  );
}

function CapabilityPanel({ caps }) {
  const sys = caps.system;
  return (
    <div class="card cap-panel">
      <h3>{t('capabilities.title')}</h3>
      <div class="cap-grid">
        <div class="cap-item">
          <span class="cap-label">{t('capabilities.cpu')}</span>
          <span class="cap-value">{sys.cpu_model}</span>
          <span class="cap-sub">{sys.cpu_physical_cores}{t('capabilities.cores')} · {sys.cpu_cores} threads · {sys.simd.join(', ')}</span>
        </div>
        <div class="cap-item">
          <span class="cap-label">{t('capabilities.memory')}</span>
          <span class="cap-value">{Math.round(sys.memory_mib / 1024 * 10) / 10} GB</span>
        </div>
        <div class="cap-item">
          <span class="cap-label">{t('capabilities.gpu')}</span>
          <span class="cap-value">{sys.gpus.map((g) => g.vendor).join(', ') || '—'}</span>
        </div>
        <div class="cap-item">
          <span class="cap-label">{t('capabilities.encoder')}</span>
          <span class="cap-value">{sys.encoder_backends.join(', ')}</span>
          <span class="cap-sub">{t('capabilities.recommended')}: {sys.recommended_quality}</span>
        </div>
      </div>
    </div>
  );
}
