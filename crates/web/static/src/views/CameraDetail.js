// Single-camera detail view: large MSE preview + controls + metadata.
import { h } from 'preact';
import { useState, useEffect } from 'preact/hooks';
import { t } from '../app.jsx';
import { api } from '../api.js';
import { streamUrls, showToast } from '../store.js';
import { navigate } from '../components/useHashRoute.js';
import { LivePreview } from '../components/LivePreview.js';
import { useStore } from '../hooks.js';

export function CameraDetail({ cameraId }) {
  const [cam, setCam] = useState(null);
  const [loading, setLoading] = useState(true);
  const [transport, setTransport] = useState('mse');
  const [recState, setRecState] = useState(null);

  useEffect(() => {
    let alive = true;
    (async () => {
      const res = await api.get(`/api/cameras/${cameraId}`);
      if (alive) { setCam(res.data); setLoading(false); }
      // Also load recording state.
      const rr = await api.get('/api/protocols/recording');
      if (alive && rr.ok) setRecState(rr.data);
    })();
    return () => { alive = false; };
  }, [cameraId]);

  const urls = useStore(streamUrls);
  const running = !!urls[cameraId];
  const rtspUrl = urls[cameraId]?.rtsp_url;

  async function start() {
    const res = await api.post(`/api/cameras/${cameraId}/start`);
    if (res.ok) {
      showToast(t('stream.started'), 'success');
      streamUrls.value = { ...streamUrls.value, [cameraId]: { rtsp_url: res.data.rtsp_url } };
    } else if (res.status !== 409) {
      showToast(t('error.failed'), 'error');
    }
  }
  async function stop() {
    const res = await api.post(`/api/cameras/${cameraId}/stop`);
    if (res.ok) {
      showToast(t('stream.stopped'), 'info');
      const next = { ...streamUrls.value };
      delete next[cameraId];
      streamUrls.value = next;
    }
  }
  async function snapshot() {
    showToast(t('snapshot.saving'), 'info');
    const res = await fetch(`/api/cameras/${cameraId}/snapshot`, { credentials: 'same-origin' });
    if (res.ok) {
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `snapshot_${cameraId}_${Date.now()}.jpg`;
      a.click();
      URL.revokeObjectURL(url);
      showToast(t('snapshot.saved'), 'success');
    } else {
      showToast(t('error.failed'), 'error');
    }
  }
  async function toggleRecord() {
    const target = !recState?.enabled;
    const res = await api.put('/api/protocols/recording', { enabled: target });
    if (res.ok) {
      setRecState({ ...recState, enabled: target });
      showToast(target ? t('record.started') : t('record.stopped'), 'info');
    }
  }
  async function copyUrl() {
    if (!rtspUrl) return;
    try { await navigator.clipboard.writeText(rtspUrl); showToast(t('camera_view.url_copied'), 'success'); }
    catch { showToast(t('error.failed'), 'error'); }
  }

  if (loading) return <div class="loading"><div class="spinner" /><span>{t('camera_view.loading')}</span></div>;
  if (!cam) return <div class="empty"><h3>{t('camera_view.load_failed')}</h3></div>;

  return (
    <section class="page">
      <div class="page-head">
        <div>
          <h1>{cam.name}</h1>
          <p class="text-muted">{cam.camera_type} · {cam.id}</p>
        </div>
        <button class="btn" onClick={() => navigate('#db')}>{t('camera_view.back')}</button>
      </div>

      <div class="camera-layout">
        <div class="camera-stage">
          {running ? (
            <LivePreview cameraId={cameraId} transport={transport} />
          ) : (
            <div class="preview-placeholder">
              <div class="play-icon">▶</div>
              <h3>{t('camera_view.no_stream')}</h3>
              <p class="text-muted">{t('camera_view.start_stream')}</p>
            </div>
          )}
          <div class="transport-toggle">
            <span>{t('camera_view.transport_label')}</span>
            <button class={transport === 'mse' ? 'active' : ''} onClick={() => setTransport('mse')}>
              {t('camera_view.transport_mse')}
            </button>
            <button class={transport === 'mjpeg' ? 'active' : ''} onClick={() => setTransport('mjpeg')}>
              {t('camera_view.transport_mjpeg')}
            </button>
          </div>
        </div>

        <div class="camera-controls">
          {running ? (
            <button class="btn btn-danger" onClick={stop}>{t('stream.stop')}</button>
          ) : (
            <button class="btn btn-primary" onClick={start}>{t('stream.start')}</button>
          )}
          <button class="btn" onClick={snapshot} disabled={!running}>{t('snapshot.btn')}</button>
          <button class={recState?.enabled ? 'btn btn-danger' : 'btn btn-primary'} onClick={toggleRecord}>
            {recState?.enabled ? t('record.stop') : t('record.start')}
          </button>
          {rtspUrl && (
            <>
              <code class="rtsp-url">{rtspUrl}</code>
              <button class="btn btn-sm" onClick={copyUrl}>{t('camera_view.copy_rtsp')}</button>
            </>
          )}
        </div>

        <div class="card info-grid">
          <div class="info-item"><span class="info-label">{t('caminfo.id')}</span><span class="info-value">{cam.id}</span></div>
          <div class="info-item"><span class="info-label">{t('caminfo.type')}</span><span class="info-value">{cam.camera_type}</span></div>
          <div class="info-item"><span class="info-label">{t('caminfo.status')}</span><span class="info-value">{running ? 'Running' : 'Stopped'}</span></div>
          <div class="info-item"><span class="info-label">{t('caminfo.created')}</span><span class="info-value">{cam.created_at}</span></div>
        </div>
      </div>
    </section>
  );
}
