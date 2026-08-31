// Live view: adaptive player + AI overlay + stream HUD.
//
// Transport fallback chain (SPEC §4.1, driven by capabilities):
//   MSE over chunked HTTP  →  MJPEG <img>  →  snapshot polling
// The MSE engine self-heals (stall timer, exponential backoff, active
// SourceBuffer pruning) and keeps the playhead pinned near live.

import { api } from './api.js';
import { store, cameraId, hasCap } from './store.js';
import { $, toast } from './ui.js';
import { t } from './i18n.js';

const MAX_BACKOFF_MS = 8000;
const STALL_TIMEOUT_MS = 10000;
const MAX_BUFFER_SECS = 8;

let engine = null;       // active transport engine (mse | mjpeg | poll)
let pollTimer = null;
let liveDotTimer = null;
let healthStamp = 0;

export function mseSupported() {
  if (!window.MediaSource || !MediaSource.isTypeSupported) return false;
  return MediaSource.isTypeSupported('video/mp4; codecs="avc1.42E01E"') ||
         MediaSource.isTypeSupported('video/mp4; codecs="avc1.4D401F"') ||
         MediaSource.isTypeSupported('video/mp4; codecs="avc1.640028"');
}

export function initLive() {
  $('btn-fullscreen').addEventListener('click', () => {
    const wrapper = document.querySelector('.stream-wrapper');
    if (document.fullscreenElement) document.exitFullscreen();
    else if (wrapper && wrapper.requestFullscreen) wrapper.requestFullscreen();
  });
  $('btn-snapshot').addEventListener('click', captureSnapshot);
  $('btn-hflip').addEventListener('click', () => {
    store.hflip = !store.hflip;
    localStorage.setItem('mibee_hflip', store.hflip ? '1' : '0');
    applyTransform();
  });
  $('btn-vflip').addEventListener('click', () => {
    store.vflip = !store.vflip;
    localStorage.setItem('mibee_vflip', store.vflip ? '1' : '0');
    applyTransform();
  });
  $('stream-retry').addEventListener('click', startLive);
  const sel = $('live-camera-select');
  if (sel) sel.addEventListener('change', () => {
    store.currentCameraId = sel.value || null;
    startLive();
  });
  // Camera selector only exists for multi-camera devices.
  $('live-camera-field').classList.toggle('hidden', !hasCap('multi_camera'));

  // Live clock
  setInterval(() => {
    const clock = $('stream-clock');
    if (clock) clock.textContent = new Date().toLocaleTimeString('en-GB', { hour12: false });
  }, 1000);
  applyTransform();
}

/// Populate the camera <select> from store.cameras (multi-camera only).
export function refreshCameraSelect() {
  const sel = $('live-camera-select');
  if (!sel || !hasCap('multi_camera')) return;
  const prev = cameraId();
  sel.innerHTML = '';
  for (const c of store.cameras) {
    const opt = document.createElement('option');
    opt.value = c.id;
    opt.textContent = c.name || c.id;
    if (c.id === prev) opt.selected = true;
    sel.appendChild(opt);
  }
  store.currentCameraId = sel.value || store.currentCameraId;
}

export function startLive() {
  stopLive();
  $('stream-error').classList.add('hidden');
  $('stream-loading').classList.remove('hidden');
  setLoadingLabel(false);
  $('mjpeg-fallback-badge').classList.add('hidden');

  const id = cameraId();
  if (hasCap('mse') && mseSupported()) {
    engine = startMse(id, () => {
      // MSE gave up — fall through the chain.
      if (hasCap('mjpeg')) engine = startMjpeg(id);
      else engine = startPolling(id);
    });
  } else if (hasCap('mjpeg')) {
    engine = startMjpeg(id);
  } else {
    engine = startPolling(id);
  }
}

export function stopLive() {
  if (engine) { engine.stop(); engine = null; }
  if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
}

// ─── MSE engine ────────────────────────────────────────────────────
function startMse(cameraIdArg, onGiveUp) {
  const video = $('stream-video');
  const state = { dead: false, retries: 0 };
  let ms = null, sb = null, queue = [], abort = null, stall = null, pruning = false;

  function stop() {
    state.dead = true;
    if (abort) { try { abort.abort(); } catch (_) { /* already closed */ } abort = null; }
    if (stall) { clearTimeout(stall); stall = null; }
    if (sb) { try { sb.onupdateend = null; sb.onerror = null; } catch (_) { /* detached */ } }
    if (ms && ms.readyState === 'open') { try { ms.endOfStream(); } catch (_) { /* ignore */ } }
    ms = null; sb = null; queue = []; pruning = false;
  }

  function reconnect() {
    stop();
    if (state.dead) return;
    state.retries += 1;
    const backoff = Math.min(MAX_BACKOFF_MS, 500 * 2 ** Math.min(state.retries - 1, 4));
    setLoadingLabel(true);
    $('stream-loading').classList.remove('hidden');
    setTimeout(() => { if (!state.dead) start(); }, backoff);
  }

  function resetStall() {
    if (stall) clearTimeout(stall);
    stall = setTimeout(() => reconnect(), STALL_TIMEOUT_MS);
  }

  function start() {
    resetStall();
    try { ms = new MediaSource(); } catch (_) { onGiveUp(); return; }
    video.src = URL.createObjectURL(ms);
    video.load(); // force the element to pick up the new src on reconnect
    ms.addEventListener('sourceopen', onOpen, { once: true });
  }

  function onOpen() {
    if (state.dead) return;
    // Init segment arrives first; codec string is parsed from its avcC box.
    sb = null;
    const initBuffer = [];
    let initDone = false;
    let sourceBuffer = null;

    async function pump() {
      if (state.dead) return;
      if (!sourceBuffer || sourceBuffer.updating) return;

      // Prune behind the playhead so the buffer never saturates.
      if (!pruning && video.buffered.length > 0) {
        const cur = video.currentTime;
        for (let i = 0; i < video.buffered.length; i++) {
          const s = video.buffered.start(i), e = video.buffered.end(i);
          if (cur >= s && cur <= e && cur - s > MAX_BUFFER_SECS) {
            const to = cur - MAX_BUFFER_SECS / 2;
            if (to > s) {
              pruning = true;
              try { sourceBuffer.remove(s, to); return; } catch (_) { pruning = false; }
            }
            break;
          }
        }
      }

      const chunk = queue.shift();
      if (!chunk) return;
      try {
        sourceBuffer.appendBuffer(chunk);
        bumpLiveDot();
        state.retries = 0;
        if (initDone) {
          $('stream-loading').classList.add('hidden');
          updateHealth();
        }
        // Pin the playhead near live.
        if (video.buffered.length > 0) {
          const first = video.buffered.start(0);
          const end = video.buffered.end(video.buffered.length - 1);
          // The stream's media timestamps need not begin at 0 — if the
          // playhead sits before any buffered data it can never start.
          if (video.currentTime < first) video.currentTime = first + 0.05;
          const lag = end - video.currentTime;
          updateHealth(lag);
          if (lag > MAX_BUFFER_SECS) video.currentTime = end - 0.3;
          else if (lag > 0.5) video.playbackRate = Math.min(1.5, 1.0 + lag * 0.2);
          else video.playbackRate = 1.0;
        }
        // Muted autoplay is allowed, but some browsers still need a nudge.
        if (video.paused) video.play().catch(() => { /* stall timer will retry */ });
      } catch (e) {
        if (e.name === 'QuotaExceededError') {
          queue.unshift(chunk);
          if (sourceBuffer.buffered.length > 0 && !pruning) {
            pruning = true;
            const s0 = sourceBuffer.buffered.start(0);
            const to = sourceBuffer.buffered.end(sourceBuffer.buffered.length - 1) - 2;
            if (to > s0) { try { sourceBuffer.remove(s0, to); return; } catch (_) { pruning = false; } }
          }
        } else {
          reconnect();
        }
      }
    }

    async function fetchStream() {
      const controller = new AbortController();
      abort = controller;
      try {
        const resp = await fetch(`/api/cameras/${cameraIdArg}/stream.mse`, {
          credentials: 'same-origin',
          signal: controller.signal,
        });
        if (!resp.ok || !resp.body) { reconnect(); return; }
        const reader = resp.body.getReader();
        for (;;) {
          if (state.dead) return;
          const { done, value } = await reader.read();
          if (done) { reconnect(); return; }
          if (!value || !value.length) continue;
          resetStall();
          if (!sourceBuffer) {
            initBuffer.push(value);
            // First chunk(s) = init segment; parse avcC for the codec string.
            const merged = concat(initBuffer);
            const codec = codecFromInit(merged) || 'video/mp4; codecs="avc1.42E01E"';
            try {
              sourceBuffer = ms.addSourceBuffer(codec);
            } catch (_) {
              try { sourceBuffer = ms.addSourceBuffer('video/mp4; codecs="avc1.4D401F"'); }
              catch (_) { onGiveUp(); return; }
            }
            sb = sourceBuffer;
            sourceBuffer.mode = 'segments';
            sourceBuffer.addEventListener('updateend', () => { pruning = false; pump(); });
            sourceBuffer.addEventListener('error', () => reconnect());
            queue.push(merged);
            initDone = true;
            pump();
          } else {
            queue.push(value);
            pump();
          }
        }
      } catch (e) {
        if (state.dead || e.name === 'AbortError') return;
        reconnect();
      }
    }

    fetchStream();
  }

  start();
  return { stop };
}

function concat(chunks) {
  let len = 0;
  for (const c of chunks) len += c.length;
  const out = new Uint8Array(len);
  let off = 0;
  for (const c of chunks) { out.set(c, off); off += c.length; }
  return out;
}

/// Derive the avc1.PPCCLL codec string from an fMP4 init segment's avcC box.
function codecFromInit(data) {
  for (let i = 0; i < data.length - 12; i++) {
    if (data[i + 4] === 0x61 && data[i + 5] === 0x76 && data[i + 6] === 0x63 && data[i + 7] === 0x43) {
      const hex = (v) => v.toString(16).padStart(2, '0');
      return 'video/mp4; codecs="avc1.' + hex(data[i + 9]) + hex(data[i + 10]) + hex(data[i + 11]) + '"';
    }
  }
  return null;
}

// ─── MJPEG fallback ────────────────────────────────────────────────
function startMjpeg(cameraIdArg) {
  const video = $('stream-video');
  const img = $('stream-img');
  video.classList.add('hidden');
  img.classList.remove('hidden');
  $('mjpeg-fallback-badge').classList.remove('hidden');
  $('stream-loading').classList.add('hidden');
  img.src = `/api/cameras/${cameraIdArg}/live?_=${Date.now()}`;
  img.onerror = () => { img.src = ''; setTimeout(() => { img.src = `/api/cameras/${cameraIdArg}/live?_=${Date.now()}`; }, 3000); };
  bumpLiveDot();
  return {
    stop() {
      img.src = '';
      img.classList.add('hidden');
      video.classList.remove('hidden');
    },
  };
}

// ─── Snapshot polling (last resort, e.g. Go without MJPEG) ─────────
function startPolling(cameraIdArg) {
  const video = $('stream-video');
  const img = $('stream-img');
  video.classList.add('hidden');
  img.classList.remove('hidden');
  const tick = () => { img.src = `/api/cameras/${cameraIdArg}/snapshot?_=${Date.now()}`; };
  tick();
  pollTimer = setInterval(tick, 5000);
  return {
    stop() {
      if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
      img.src = '';
      img.classList.add('hidden');
      video.classList.remove('hidden');
    },
  };
}

// ─── HUD / helpers ─────────────────────────────────────────────────
function bumpLiveDot() {
  const dot = $('stream-live-dot');
  if (!dot) return;
  dot.classList.add('active');
  clearTimeout(liveDotTimer);
  liveDotTimer = setTimeout(() => dot.classList.remove('active'), 1500);
}

function updateHealth(lag) {
  const now = Date.now();
  if (now - healthStamp < 1000) return;
  healthStamp = now;
  const el = $('stream-health');
  const video = $('stream-video');
  if (!el || !video) return;
  const w = video.videoWidth || 0, h = video.videoHeight || 0;
  el.textContent = (w && h ? w + '\u00d7' + h : '--\u00d7--') +
    (lag !== undefined ? ' \u00b7 ' + lag.toFixed(1) + 's' : '');
  el.className = 'stream-health ' + (lag === undefined ? '' : lag < 0.5 ? 'health-good' : lag < 2.0 ? 'health-warn' : 'health-bad');
}

function setLoadingLabel(reconnecting) {
  const label = $('stream-loading-label');
  if (label) label.textContent = reconnecting ? t('reconnecting') : t('loading');
}

export function showStreamError(msgKey) {
  const label = $('stream-error-label');
  if (label) label.textContent = t(msgKey || 'streamError');
  $('stream-error').classList.remove('hidden');
  $('stream-loading').classList.add('hidden');
}

export function applyTransform() {
  const video = $('stream-video');
  if (!video) return;
  const cam = (store.config && store.config.camera) || {};
  const rot = parseInt(cam.rotation, 10) || 0;
  const parts = [];
  if (rot) parts.push('rotate(' + rot + 'deg)');
  if (store.hflip) parts.push('scaleX(-1)');
  if (store.vflip) parts.push('scaleY(-1)');
  video.style.transform = parts.length ? parts.join(' ') : '';
  const overlay = $('detection-overlay');
  if (overlay) overlay.style.transform = video.style.transform;
  const hBtn = $('btn-hflip'), vBtn = $('btn-vflip');
  if (hBtn) hBtn.setAttribute('aria-pressed', String(store.hflip));
  if (vBtn) vBtn.setAttribute('aria-pressed', String(store.vflip));
}

function captureSnapshot() {
  const video = $('stream-video');
  const img = $('stream-img');
  const source = (!video.classList.contains('hidden') && video.videoWidth) ? video : null;
  if (!source && (img.classList.contains('hidden') || !img.naturalWidth)) {
    toast(t('snapshotFail'), 'error');
    return;
  }
  const w = source ? source.videoWidth : img.naturalWidth;
  const h = source ? source.videoHeight : img.naturalHeight;
  const cam = (store.config && store.config.camera) || {};
  const rot = parseInt(cam.rotation, 10) || 0;
  const canvas = document.createElement('canvas');
  if (rot === 90 || rot === 270) { canvas.width = h; canvas.height = w; }
  else { canvas.width = w; canvas.height = h; }
  const ctx = canvas.getContext('2d');
  ctx.translate(canvas.width / 2, canvas.height / 2);
  ctx.rotate(rot * Math.PI / 180);
  if (store.hflip) ctx.scale(-1, 1);
  if (store.vflip) ctx.scale(1, -1);
  ctx.drawImage(source || img, -w / 2, -h / 2);
  const a = document.createElement('a');
  a.download = 'mibee-' + Date.now() + '.jpg';
  try {
    a.href = canvas.toDataURL('image/jpeg', 0.9);
    a.click();
  } catch (_) {
    toast(t('snapshotFail'), 'error');
  }
}

// ─── AI detection overlay (extension: ai) ──────────────────────────
let detectionClearTimer = null;

export function renderDetections(dets) {
  const canvas = $('detection-overlay');
  const video = $('stream-video');
  const wrapper = document.querySelector('.stream-wrapper');
  if (!canvas || !video || !wrapper || !video.videoWidth) return;
  // When the MJPEG/polling fallback is active there is no <video> geometry —
  // map onto the <img> instead.
  const media = video.classList.contains('hidden') ? $('stream-img') : video;
  if (!media) return;

  const dpr = window.devicePixelRatio || 1;
  const w = wrapper.clientWidth, h = wrapper.clientHeight;
  if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
  }
  const ctx = canvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const vw = media.videoWidth || media.naturalWidth || 0;
  const vh = media.videoHeight || media.naturalHeight || 0;
  if (!vw || !vh) return;
  const scale = Math.min(w / vw, h / vh);
  const ox = (w - vw * scale) / 2, oy = (h - vh * scale) / 2;

  const color = getComputedStyle(canvas).getPropertyValue('--detection-color').trim() || '#00c8a0';
  const labelBg = getComputedStyle(canvas).getPropertyValue('--detection-label-bg').trim() || 'rgba(0,0,0,.55)';
  const mono = getComputedStyle(document.documentElement).getPropertyValue('--font-mono').trim() || 'monospace';
  ctx.lineWidth = 2;
  ctx.strokeStyle = color;
  ctx.font = '11px ' + mono;

  for (const d of dets) {
    const b = d.bbox || [];
    if (b.length < 4) continue;
    const x = ox + b[0] * scale, y = oy + b[1] * scale;
    const bw = b[2] * scale, bh = b[3] * scale;
    ctx.strokeRect(x, y, bw, bh);
    const label = (d.label || '?') + ' ' + Math.round((d.confidence || 0) * 100) + '%';
    const tw = ctx.measureText(label).width;
    ctx.fillStyle = labelBg;
    ctx.fillRect(x, y - 16, tw + 8, 16);
    ctx.fillStyle = color;
    ctx.fillText(label, x + 4, y - 4);
  }

  clearTimeout(detectionClearTimer);
  detectionClearTimer = setTimeout(() => ctx.clearRect(0, 0, w, h), 2000);
}
