// LivePreview — adaptive H.264 (MSE) video tile with MJPEG fallback.
//
// The component tries MSE first (low bandwidth, hardware decode) and falls
// back to the legacy MJPEG <img> endpoint when MSE is unavailable or the
// SourceBuffer rejects the codec. Each instance owns its own MediaSource so
// multiple tiles in a grid are independent.
//
// Robustness: the MSE fetch + append loop self-heals — on any stream error,
// reader EOF, or SourceBuffer issue the component tears down its MediaSource
// and reconnects with exponential backoff. A stall timer fires if no chunk
// arrives within the window. The SourceBuffer is actively pruned so buffered
// data doesn't grow unbounded and freeze playback.

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

const MAX_BACKOFF_MS = 8000;
/// If no chunk arrives within this window, assume the stream stalled.
const STALL_TIMEOUT_MS = 10000;
/// How much buffered video (seconds) to keep ahead of the playhead. Pruning
/// behind this keeps memory bounded so the SourceBuffer never saturates and
/// freezes the picture.
const MAX_BUFFER_SECS = 8;

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

  useEffect(() => {
    if (transport === 'mse' && !mseSupported()) {
      setEffective(autoMjpegFallback ? 'mjpeg' : 'mse');
    } else {
      setEffective(transport);
    }
  }, [transport, autoMjpegFallback]);

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
        ref={videoRef}
        autoplay
        muted
        playsinline
        onPlaying={() => setStatus('live')}
        onWaiting={() => setStatus('connecting')}
        onError={() => { if (autoMjpegFallback) setEffective('mjpeg'); }}
      />
      <MseEngine videoRef={videoRef} cameraId={cameraId} setStatus={setStatus}
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

/// Internal: drives the MSE fetch+append loop with self-healing + active
/// SourceBuffer pruning. Headless (renders null); operates on the video
/// element via ref.
function MseEngine({ videoRef, cameraId, setStatus, onGiveUp }) {
  const msRef = useRef(null);
  const sbRef = useRef(null);
  const queueRef = useRef([]);
  const abortRef = useRef(null);
  const stallTimerRef = useRef(null);
  const deadRef = useRef(false);
  const retryCountRef = useRef(0);
  const pruningRef = useRef(false);

  useEffect(() => {
    deadRef.current = false;
    startSession();
    return () => {
      deadRef.current = true;
      cleanup();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cameraId]);

  function cleanup() {
    if (abortRef.current) { try { abortRef.current.abort(); } catch {} abortRef.current = null; }
    if (stallTimerRef.current) { clearTimeout(stallTimerRef.current); stallTimerRef.current = null; }
    const sb = sbRef.current;
    if (sb) { try { sb.onupdateend = null; sb.onerror = null; } catch {} }
    const ms = msRef.current;
    if (ms && ms.readyState === 'open') { try { ms.endOfStream(); } catch {} }
    msRef.current = null;
    sbRef.current = null;
    queueRef.current = [];
    pruningRef.current = false;
  }

  function resetStallTimer() {
    if (stallTimerRef.current) clearTimeout(stallTimerRef.current);
    stallTimerRef.current = setTimeout(() => {
      console.warn('[mse] stall — no chunks for', STALL_TIMEOUT_MS + 'ms, reconnecting');
      reconnect();
    }, STALL_TIMEOUT_MS);
  }

  function reconnect() {
    cleanup();
    if (deadRef.current) return;
    retryCountRef.current += 1;
    const backoff = Math.min(MAX_BACKOFF_MS, 500 * 2 ** Math.min(retryCountRef.current - 1, 4));
    setStatus('connecting');
    setTimeout(() => { if (!deadRef.current) startSession(); }, backoff);
  }

  function startSession() {
    const video = videoRef.current;
    if (!video || deadRef.current) return;

    // Start the stall timer immediately so a sourceopen/fetch that never
    // delivers still triggers a reconnect.
    resetStallTimer();

    let ms;
    try { ms = new MediaSource(); } catch { onGiveUp(); return; }
    msRef.current = ms;
    const url = URL.createObjectURL(ms);
    video.src = url;
    // Force the element to pick up the new src — without this, a reconnect
    // after a previous endOfStream can leave the video frozen on the old frame.
    video.load();

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
      sb.addEventListener('updateend', onSbUpdateEnd);
      sb.addEventListener('error', onSbError);
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
          for (;;) {
            if (deadRef.current) return;
            const { done, value } = await reader.read();
            if (done) { reconnect(); return; }
            if (value && value.length) {
              queueRef.current.push(value);
              pump();
              resetStallTimer();
              retryCountRef.current = 0;
            }
          }
        } catch (e) {
          if (deadRef.current) return;
          if (e.name === 'AbortError') return;
          console.warn('[mse] fetch error, reconnecting:', e);
          reconnect();
        }
      })();
    }

    function onSbUpdateEnd() {
      pruningRef.current = false;
      pump();
    }

    function onSbError() {
      console.warn('[mse] SourceBuffer error, reconnecting');
      reconnect();
    }

    /// Append the next queued chunk if the SourceBuffer is idle, and prune the
    /// buffered range when it grows past MAX_BUFFER_SECS so the buffer never
    /// saturates and freezes playback.
    function pump() {
      const sb = sbRef.current;
      if (!sb || sb.updating || deadRef.current) return;

      // Active pruning: keep at most MAX_BUFFER_SECS behind the current
      // position. This is the key fix for the "frozen frame" issue — without
      // pruning, the SourceBuffer accumulates the entire stream and the
      // browser eventually stops accepting new data.
      const video = videoRef.current;
      if (!pruningRef.current && video && sb.buffered.length > 0) {
        const current = video.currentTime;
        // Find the start of the buffered range we're playing in.
        for (let i = 0; i < sb.buffered.length; i++) {
          const start = sb.buffered.start(i);
          const end = sb.buffered.end(i);
          if (current >= start && current <= end) {
            if (current - start > MAX_BUFFER_SECS) {
              const removeEnd = current - MAX_BUFFER_SECS / 2;
              if (removeEnd > start) {
                pruningRef.current = true;
                try { sb.remove(start, removeEnd); return; }
                catch (e) { pruningRef.current = false; }
              }
            }
            break;
          }
        }
      }

      if (queueRef.current.length === 0) return;
      const chunk = queueRef.current.shift();
      try {
        sb.appendBuffer(chunk);
        setStatus('live');
        // Nudge the playhead forward if it lags too far behind live (common
        // after a transient stall) — otherwise the video element pauses on a
        // stale frame and never catches up.
        if (video && sb.buffered.length > 0) {
          const end = sb.buffered.end(sb.buffered.length - 1);
          if (end - video.currentTime > MAX_BUFFER_SECS) {
            video.currentTime = end - 0.3;
          }
        }
      } catch (e) {
        if (e.name === 'QuotaExceededError') {
          // Buffer full — force a prune of the oldest range and requeue.
          if (sb.buffered.length > 0 && !pruningRef.current) {
            pruningRef.current = true;
            const start = sb.buffered.start(0);
            const pruneTo = sb.buffered.end(sb.buffered.length - 1) - 2;
            if (pruneTo > start) {
              try { sb.remove(start, pruneTo); queueRef.current.unshift(chunk); return; }
              catch { pruningRef.current = false; }
            }
          }
          queueRef.current.unshift(chunk);
        } else {
          console.warn('[mse] appendBuffer error, reconnecting:', e);
          reconnect();
        }
      }
    }
  }

  return null;
}
