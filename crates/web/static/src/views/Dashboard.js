// Dashboard view: camera management table + multi-tile live grid.
//
// Two modes:
//   - "table" (default): the existing CRUD table with Start/Stop/View/Edit/Delete
//   - "grid": 1/4/9 tile live preview using MSE <video> for every active camera
//
// The grid lets the user see multiple cameras simultaneously — the headline
// NVR feature. The table remains for fine-grained management.

import { h } from 'preact';
import { useState, useEffect } from 'preact/hooks';
import { t } from '../app.jsx';
import { api } from '../api.js';
import { cameras, streamUrls, showToast } from '../store.js';
import { navigate } from '../components/useHashRoute.js';
import { LivePreview } from '../components/LivePreview.js';

const GRID_LAYOUTS = [
  { id: 1, label: 'grid.layout_1', cols: '1fr', count: 1 },
  { id: 4, label: 'grid.layout_4', cols: '1fr 1fr', count: 4 },
  { id: 9, label: 'grid.layout_9', cols: '1fr 1fr 1fr', count: 9 },
];

export function Dashboard({ onEditCamera, onAddCamera, onDeleteCamera }) {
  const [mode, setMode] = useState('table');
  const [layout, setLayout] = useState(4);
  const [loading, setLoading] = useState(true);

  // Initial load + periodic refresh of the camera list.
  useEffect(() => {
    let alive = true;
    (async () => {
      await refreshCameras();
      if (alive) setLoading(false);
    })();
    const interval = setInterval(refreshCameras, 10000);
    return () => { alive = false; clearInterval(interval); };
  }, []);

  async function refreshCameras() {
    const res = await api.get('/api/cameras');
    if (res.ok && res.data) {
      cameras.value = res.data;
      // Build the running-stream map from rtsp_url presence.
      const urls = {};
      for (const c of res.data) {
        if (c.rtsp_url) urls[c.id] = { rtsp_url: c.rtsp_url, status: 'running' };
      }
      streamUrls.value = urls;
    }
  }

  async function startStream(id) {
    const res = await api.post(`/api/cameras/${id}/start`);
    if (res.ok) {
      showToast(t('stream.started'), 'success');
      streamUrls.value = { ...streamUrls.value, [id]: { rtsp_url: res.data.rtsp_url, status: 'running' } };
    } else if (res.status !== 409) {
      showToast(t('error.failed') + ': ' + (res.data?.error || ''), 'error');
    }
  }
  async function stopStream(id) {
    const res = await api.post(`/api/cameras/${id}/stop`);
    if (res.ok) {
      showToast(t('stream.stopped'), 'info');
      const next = { ...streamUrls.value };
      delete next[id];
      streamUrls.value = next;
    }
  }

  const camList = cameras.value;
  const runningIds = Object.keys(streamUrls.value);

  return (
    <section class="page">
      <div class="page-head">
        <div>
          <h1>{t('cameras.title')}</h1>
          <p class="text-muted">{t('cameras.subtitle')}</p>
        </div>
        <div class="head-actions">
          <div class="mode-switch" role="tablist" aria-label="View mode">
            <button role="tab" class={mode === 'table' ? 'active' : ''}
                    aria-selected={mode === 'table'} onClick={() => setMode('table')}>▮</button>
            <button role="tab" class={mode === 'grid' ? 'active' : ''}
                    aria-selected={mode === 'grid'} onClick={() => setMode('grid')}>▦</button>
          </div>
          {mode === 'grid' && (
            <select class="layout-select" value={layout}
                    onChange={(e) => setLayout(Number(e.target.value))}>
              {GRID_LAYOUTS.map((l) => (
                <option value={l.id}>{t(l.label)}</option>
              ))}
            </select>
          )}
          <button class="btn btn-primary" onClick={onAddCamera}>{t('cameras.add')}</button>
        </div>
      </div>

      {loading ? (
        <div class="loading"><div class="spinner" /><span>{t('cameras.loading')}</span></div>
      ) : camList.length === 0 ? (
        <div class="empty">
          <div class="empty-icon">📷</div>
          <h3>{t('cameras.empty_title')}</h3>
          <p class="text-muted">{t('cameras.empty_desc')}</p>
        </div>
      ) : mode === 'grid' ? (
        <LiveGrid
          cameras={camList}
          runningIds={runningIds}
          layout={layout}
          onStart={startStream}
          onStop={stopStream}
          onView={(id) => navigate('#/c/' + id)}
        />
      ) : (
        <CameraTable
          cameras={camList}
          runningIds={runningIds}
          onStart={startStream}
          onStop={stopStream}
          onView={(id) => navigate('#/c/' + id)}
          onEdit={onEditCamera}
          onDelete={onDeleteCamera}
        />
      )}
    </section>
  );
}

function CameraTable({ cameras, runningIds, onStart, onStop, onView, onEdit, onDelete }) {
  return (
    <div class="card">
      <table class="data-table">
        <thead>
          <tr>
            <th>{t('table.name')}</th>
            <th>{t('table.type')}</th>
            <th>{t('table.status')}</th>
            <th>{t('table.stream_url')}</th>
            <th>{t('table.actions')}</th>
          </tr>
        </thead>
        <tbody>
          {cameras.map((c) => {
            const running = runningIds.includes(c.id);
            return (
              <tr key={c.id}>
                <td><a href={'#/c/' + c.id}>{c.name}</a></td>
                <td>{c.camera_type}</td>
                <td>
                  <span class={running ? 'badge badge-ok' : 'badge badge-muted'}>
                    {running ? 'Running' : c.status === 'offline' ? t('camera.offline') : 'Stopped'}
                  </span>
                </td>
                <td class="url-cell">
                  {c.rtsp_url ? (
                    <>
                      <code>{c.rtsp_url}</code>
                      <button class="btn-icon" title={t('common.copy')}
                              onClick={() => copyText(c.rtsp_url)}>⧉</button>
                    </>
                  ) : <span class="text-muted">{t('table.url_unavailable')}</span>}
                </td>
                <td class="actions-cell">
                  {running ? (
                    <button class="btn btn-sm btn-danger" onClick={() => onStop(c.id)}>{t('stream.stop')}</button>
                  ) : (
                    <button class="btn btn-sm btn-primary" onClick={() => onStart(c.id)}>{t('stream.start')}</button>
                  )}
                  <button class="btn btn-sm" onClick={() => onView(c.id)}>{t('stream.view')}</button>
                  <button class="btn btn-sm" onClick={() => onEdit(c)}>{t('stream.edit')}</button>
                  <button class="btn btn-sm btn-danger" onClick={() => onDelete(c)}>{t('stream.delete')}</button>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function LiveGrid({ cameras, runningIds, layout, onStart, onStop, onView }) {
  const cfg = GRID_LAYOUTS.find((l) => l.id === layout) || GRID_LAYOUTS[1];
  const activeCams = cameras.filter((c) => runningIds.includes(c.id));

  return (
    <div>
      {activeCams.length === 0 ? (
        <div class="empty">
          <div class="empty-icon">▦</div>
          <h3>{t('grid.empty')}</h3>
        </div>
      ) : (
        <div class="live-grid" style={`grid-template-columns:${cfg.cols}`}>
          {activeCams.slice(0, cfg.count).map((c) => (
            <div class="grid-tile" key={c.id} onDoubleClick={() => onView(c.id)}>
              <LivePreview cameraId={c.id} />
              <div class="tile-overlay">
                <span class="tile-name">{c.name}</span>
                <button class="btn-icon" title={t('stream.stop')} onClick={(e) => { e.stopPropagation(); onStop(c.id); }}>■</button>
              </div>
            </div>
          ))}
        </div>
      )}
      {/* Inactive cameras get Start buttons below the grid. */}
      {cameras.filter((c) => !runningIds.includes(c.id)).length > 0 && (
        <div class="card inactive-row">
          <h4>{t('stream.start')}</h4>
          <div class="inactive-list">
            {cameras.filter((c) => !runningIds.includes(c.id)).map((c) => (
              <button key={c.id} class="btn btn-sm" onClick={() => onStart(c.id)}>
                ▶ {c.name}
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    showToast(t('camera_view.url_copied_short'), 'success');
  } catch {
    const ta = document.createElement('textarea');
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand('copy'); showToast(t('camera_view.url_copied_short'), 'success'); }
    catch {}
    document.body.removeChild(ta);
  }
}
