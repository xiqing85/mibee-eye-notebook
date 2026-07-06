// LivePreview — adaptive H.264 (MSE) video tile with MJPEG fallback.
//
// The component tries MSE first (low bandwidth, hardware decode) and falls
// back to the legacy MJPEG <img> endpoint when MSE is unavailable or the
// SourceBuffer rejects the codec. Each instance owns its own MediaSource so
// multiple tiles in a grid are independent.

import { h } from 'preact';
import { useEffect, useRef, useState } from 'preact/hooks';
import { t } from '../app.jsx';

/// Probe whether this browser can play H.264 via MSE. Cached after first call.
function mseSupported() {
  if (!window.MediaSource || !MediaSource.isTypeSupported) return false;
  // Baseline/High profile H.264, used by OpenH264.
  return MediaSource.isTypeSupported('video/mp4; codecs="avc1.42E01E"') ||
         MediaSource.isTypeSupported('video/mp4; codecs="avc1.4D401F"') ||
         MediaSource.isTypeSupported('video/mp4; codecs="avc1.640028"');
}

/**
 * @param {object} props
 * @param {string} props.cameraId
 * @param {string} [props.transport] — 'mse' (default) or 'mjpeg'
 * @param {boolean} [props.autoMjpegFallback] — fall back to MJPEG on MSE error
 */
export function LivePreview({ cameraId, transport = 'mse', autoMjpegFallback = true }) {
  const videoRef = useRef(null);
  const [effective, setEffective] = useState(transport);
  const [status, setStatus] = useState('connecting');
  const mseRef = useRef(null);
  const sourceBufRef = useRef(null);
  const queueRef = useRef([]);
  const abortedRef = useRef(false);

  // Decide effective transport upfront.
  useEffect(() => {
    if (transport === 'mse' && !mseSupported()) {
      setEffective(autoMjpegFallback ? 'mjpeg' : 'mse');
    } else {
      setEffective(transport);
    }
  }, [transport, autoMjpegFallback]);

  // MSE plumbing: open a MediaSource, attach a SourceBuffer fed by the
  // /stream.mse chunked endpoint, drain the queue as the buffer allows.
  useEffect(() => {
    if (effective !== 'mse') return;
    abortedRef.current = false;
    setStatus('connecting');

    const video = videoRef.current;
    if (!video) return;

    const ms = new MediaSource();
    mseRef.current = ms;
    video.src = URL.createObjectURL(ms);

    ms.addEventListener('sourceopen', () => {
      if (abortedRef.current) return;
      let sb;
      try {
        sb = ms.addSourceBuffer('video/mp4; codecs="avc1.42E01E"');
      } catch {
        try {
          sb = ms.addSourceBuffer('video/mp4; codecs="avc1.4D401F"');
        } catch {
          // Codec unsupported — fall back to MJPEG if allowed.
          if (autoMjpegFallback) setEffective('mjpeg');
          return;
        }
      }
      sourceBufRef.current = sb;
      sb.mode = 'segments';
      sb.addEventListener('updateend', drainQueue);
      startFetch();
    });

    let controller = new AbortController();
    async function startFetch() {
      try {
        const resp = await fetch(`/api/cameras/${cameraId}/stream.mse`, {
          credentials: 'same-origin',
          signal: controller.signal,
        });
        if (!resp.ok || !resp.body) {
          if (autoMjpegFallback) setEffective('mjpeg');
          return;
        }
        const reader = resp.body.getReader();
        // Append incoming chunks; maintain a queue so we never call
        // appendBuffer while an update is in flight.
        while (true) {
          const { done, value } = await reader.read();
          if (done) break;
          if (abortedRef.current) break;
          if (value && value.length) {
            queueRef.current.push(value);
            drainQueue();
          }
        }
      } catch (e) {
        if (e.name !== 'AbortError' && autoMjpegFallback) {
          setEffective('mjpeg');
        }
      }
    }
    function drainQueue() {
      const sb = sourceBufRef.current;
      if (!sb || sb.updating || queueRef.current.length === 0) return;
      // Coalesce small chunks into a single append for efficiency.
      const chunk = queueRef.current.shift();
      try {
        sb.appendBuffer(chunk);
        setStatus('live');
      } catch {
        // QuotaExceeded or similar — drop oldest, keep going.
        try { sb.remove(0, sb.buffered.length - 2); } catch {}
      }
    }

    return () => {
      abortedRef.current = true;
      controller.abort();
      try { ms.endOfStream(); } catch {}
      queueRef.current = [];
    };
  }, [effective, cameraId, autoMjpegFallback]);

  // MJPEG path: just point an <img> at the multipart endpoint.
  if (effective === 'mjpeg') {
    return (
      <div class="preview-tile mjpeg">
        <img
          src={`/api/cameras/${cameraId}/live`}
          alt={t('camera_view.title')}
          onError={(e) => { setStatus('offline'); }}
          onLoad={() => setStatus('live')}
        />
        <span class="preview-badge">{t('preview.fallback_mjpeg')}</span>
      </div>
    );
  }

  return (
    <div class={`preview-tile mse status-${status}`}>
      <video
        ref={videoRef}
        autoplay
        muted
        playsinline
        onPlaying={() => setStatus('live')}
        onWaiting={() => setStatus('connecting')}
        onError={() => { if (autoMjpegFallback) setEffective('mjpeg'); }}
      />
      {status !== 'live' && (
        <div class="preview-overlay">
          <div class="spinner" />
          <span>{t('preview.connecting')}</span>
        </div>
      )}
    </div>
  );
}
