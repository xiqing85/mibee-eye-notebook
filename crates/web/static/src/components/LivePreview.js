// LivePreview — adaptive H.264 (MSE) video tile with MJPEG fallback.
//
// The component tries MSE first (low bandwidth, hardware decode) and falls
// back to the legacy MJPEG <img> endpoint when MSE is unavailable or the
// SourceBuffer rejects the codec. Each instance owns its own MediaSource so
// multiple tiles in a grid are independent.
//
// Robustness: the MSE fetch + append loop self-heals — on any stream error,
// reader EOF, or SourceBuffer quota exhaustion the component tears down its
// MediaSource and reconnects with exponential backoff, so a transient
// hiccup doesn't permanently freeze the tile (the previous version needed a
// full page reload to recover).

import { h } from 'preact';
import { useEffect, useRef, useState } from 'preact/hooks';
import { t } from '../app.jsx';

/// Probe whether this browser can play H.264 via MSE. Cached after first call.
function mseSupported() {
  if (!window.MediaSource || !MediaSource.isTypeSupported) return false;
  return MediaSource.isTypeSupported('video/mp4; codecs="avc1.42E01E"') ||
         MediaSource.isTypeSupported('video/mp4; codecs="avc1.4D401F"') ||
         MediaSource.isTypeSupported('video/mp4; codecs="avc1.640028"');
}

/// Maximum backoff between reconnect attempts.
const MAX_BACKOFF_MS = 8000;
/// If no chunk arrives within this window, assume the stream stalled and reconnect.
const STALL_TIMEOUT_MS = 12000;

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
  const attemptRef = useRef(0);

  // Decide effective transport upfront.
  useEffect(() => {
    if (transport === 'mse' && !mseSupported()) {
      setEffective(autoMjpegFallback ? 'mjpeg' : 'mse');
    } else {
      setEffective(transport);
    }
  }, [transport, autoMjpegFallback]);

  // MJPEG path: just point an <img> at the multipart endpoint. The browser
  // handles reconnection for multipart/x-mixed-replace natively, so no extra
  // healing logic is needed here.
  if (effective === 'mjpeg') {
    return (
      <div class="preview-tile mjpeg">
        <img
          src={`/api/cameras/${cameraId}/live`}
          alt={t('camera_view.title')}
          onError={() => setStatus('offline')}
          onLoad={() => setStatus('live')}
        />
        <span class="preview-badge">{t('preview.fallback_mjpeg')}</span>
      </div>
    );
  }

  return (
    <div class={`preview-tile mse status-${status}`}>
      <video
        key={`vid-${cameraId}`}
        ref={videoRef}
        autoplay
        muted
        playsinline
        onPlaying={() => setStatus('live')}
        onWaiting={() => setStatus('connecting')}
        onError={() => { if (autoMjpegFallback) setEffective('mjpeg'); }}
      />
      <MseEngine key={`eng-${cameraId}`} videoRef={videoRef} cameraId={cameraId} setStatus={setStatus}
                 onGiveUp={() => autoMjpegFallback && setEffective('mjpeg')} />
      {status !== 'live' && (
        <div class="preview-overlay">
          <div class="spinner" />
          <span>{t('preview.connecting')}</span>
        </div>
      )}
    </div>
  );
}

/// Internal: drives the MSE fetch+append loop with self-healing. Rendered as a
/// child so it gets its own lifecycle (clean tear-down on unmount).
function MseEngine({ videoRef, cameraId, setStatus, onGiveUp }) {
  // All mutable state lives in refs so re-renders don't restart the effect.
  const msRef = useRef(null);
  const sbRef = useRef(null);
  const queueRef = useRef([]);
  const abortRef = useRef(null);
  const stallTimerRef = useRef(null);
  const deadRef = useRef(false);
  const retryCountRef = useRef(0);

  useEffect(() => {
    deadRef.current = false;
    startSession();

    return () => {
      // Component unmounting or deps changing — tear everything down.
      deadRef.current = true;
      cleanup();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cameraId]);

  function cleanup() {
    if (abortRef.current) { try { abortRef.current.abort(); } catch {} abortRef.current = null; }
    if (stallTimerRef.current) { clearTimeout(stallTimerRef.current); stallTimerRef.current = null; }
    const sb = sbRef.current;
    if (sb) {
      try { sb.onerror = null; sb.onupdateend = null; } catch {}
    }
    const ms = msRef.current;
    if (ms && ms.readyState === 'open') {
      try { ms.endOfStream(); } catch {}
    }
    msRef.current = null;
    sbRef.current = null;
    queueRef.current = [];
  }

  function resetStallTimer() {
    if (stallTimerRef.current) clearTimeout(stallTimerRef.current);
    stallTimerRef.current = setTimeout(() => {
      // No data for too long — the upstream likely died. Reconnect.
      console.warn('[mse] stall detected, reconnecting');
      reconnect();
    }, STALL_TIMEOUT_MS);
  }

  function reconnect() {
    cleanup();
    if (deadRef.current) return;
    retryCountRef.current += 1;
    const backoff = Math.min(MAX_BACKOFF_MS, 500 * 2 ** (retryCountRef.current - 1));
    setStatus(retryCountRef.current > 1 ? 'connecting' : 'connecting');
    setTimeout(() => {
      if (!deadRef.current) startSession();
    }, backoff);
  }

  function startSession() {
    const video = videoRef.current;
    if (!video || deadRef.current) return;

    let ms;
    try {
      ms = new MediaSource();
    } catch {
      onGiveUp();
      return;
    }
    msRef.current = ms;
    const url = URL.createObjectURL(ms);
    video.src = url;

    ms.addEventListener('sourceopen', onSourceOpen);

    function onSourceOpen() {
      if (deadRef.current) return;
      ms.removeEventListener('sourceopen', onSourceOpen);
      let sb;
      try {
        sb = ms.addSourceBuffer('video/mp4; codecs="avc1.42E01E"');
      } catch {
        try { sb = ms.addSourceBuffer('video/mp4; codecs="avc1.4D401F"'); }
        catch { onGiveUp(); return; }
      }
      sbRef.current = sb;
      sb.mode = 'segments';
      sb.addEventListener('updateend', drainQueue);
      sb.addEventListener('error', onSbError);
      resetStallTimer();
      startFetch();
    }

    function startFetch() {
      const controller = new AbortController();
      abortRef.current = controller;
      (async () => {
        try {
          const resp = await fetch(`/api/cameras/${cameraId}/stream.mse`, {
            credentials: 'same-origin',
            signal: controller.signal,
          });
          if (!resp.ok || !resp.body) { reconnect(); return; }
          const reader = resp.body.getReader();
          // The first chunk is the init segment; subsequent chunks are media
          // segments. Append each, maintaining the queue so we never call
          // appendBuffer while an update is in flight.
          for (;;) {
            if (deadRef.current) return;
            const { done, value } = await reader.read();
            if (done) { reconnect(); return; }
            if (value && value.length) {
              queueRef.current.push(value);
              drainQueue();
              resetStallTimer();
              // Reset the reconnect counter once we're successfully feeding
              // the decoder — the connection is healthy.
              retryCountRef.current = 0;
            }
          }
        } catch (e) {
          if (deadRef.current) return;
          if (e.name === 'AbortError') return; // intentional teardown
          console.warn('[mse] fetch error, reconnecting:', e);
          reconnect();
        }
      })();
    }

    function drainQueue() {
      const sb = sbRef.current;
      if (!sb || sb.updating || queueRef.current.length === 0) return;
      const chunk = queueRef.current.shift();
      try {
        sb.appendBuffer(chunk);
        setStatus('live');
      } catch (e) {
        if (e.name === 'QuotaExceededError') {
          // Decoder buffer full — prune everything except the latest GOP.
          try {
            const buffered = sb.buffered;
            if (buffered.length > 0) {
              const keepFrom = Math.max(0, buffered.end(buffered.length - 1) - 2);
              if (keepFrom > 0) sb.remove(0, keepFrom);
            }
          } catch {}
          // Re-queue the chunk for the next updateend.
          queueRef.current.unshift(chunk);
        } else {
          console.warn('[mse] appendBuffer error, reconnecting:', e);
          reconnect();
        }
      }
    }

    function onSbError() {
      console.warn('[mse] SourceBuffer error, reconnecting');
      reconnect();
    }
  }

  return null; // headless — only manages the video element via ref.
}
