// Real-time microphone waveform for voice dialogue (SPEC §6
// `audio_level`, notebook dialect #36). A scrolling bar strip in the chat
// panel: each SSE level sample pushes one bar; bars ease toward their
// target height so 10 Hz input still reads smooth, and the strip floors
// to a thin baseline when the room is silent.

import { hasCap } from './store.js';
import { $ } from './ui.js';
import { t } from './i18n.js';

const BARS = 36;
const HISTORY_MS = 6000; // stop easing once data is this stale

let levels = new Float32Array(BARS);
let targets = new Float32Array(BARS);
let head = 0;
let lastSampleAt = 0;
let rafId = null;
let ctx = null;

export function initWaveform() {
  const canvas = $('chat-waveform');
  if (!canvas) return;
  ctx = canvas.getContext('2d');
  updateWaveformVisibility();
}

export function updateWaveformVisibility() {
  const wrap = $('chat-waveform-wrap');
  if (wrap) wrap.classList.toggle('hidden', !hasCap('voice'));
}

/// One SSE sample: shift the history and start easing toward it.
export function handleAudioLevel(p) {
  const level = Math.max(0, Math.min(1, Number(p && p.level) || 0));
  head = (head + 1) % BARS;
  targets[head] = level;
  lastSampleAt = performance.now();
  if (rafId === null && ctx) {
    rafId = requestAnimationFrame(draw);
  }
}

function draw() {
  rafId = null;
  const canvas = ctx.canvas;
  const w = canvas.width;
  const h = canvas.height;
  const stale = performance.now() - lastSampleAt > HISTORY_MS;
  ctx.clearRect(0, 0, w, h);
  const slot = w / BARS;
  const barW = Math.max(2, Math.floor(slot * 0.62));
  const base = Math.max(2, h * 0.06); // silent floor: still visibly alive
  let anyMoving = false;
  for (let i = 0; i < BARS; i++) {
    const idx = (head + 1 + i) % BARS;
    const target = stale ? 0 : targets[idx];
    const cur = levels[idx] + (target - levels[idx]) * 0.35;
    levels[idx] = cur;
    if (Math.abs(target - cur) > 0.004) anyMoving = true;
    const bh = base + cur * (h - base - 2);
    const x = i * slot + (slot - barW) / 2;
    const y = (h - bh) / 2;
    // Louder bars lean into the accent color.
    const a = 0.35 + Math.min(0.65, cur * 1.4);
    ctx.fillStyle = cur > 0.55
      ? `rgba(0,200,160,${a})`
      : `rgba(130,145,160,${a})`;
    roundRect(x, y, barW, bh, Math.min(2, barW / 2));
  }
  if (anyMoving && !stale) rafId = requestAnimationFrame(draw);
}

function roundRect(x, y, w, h, r) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.lineTo(x + w - r, y);
  ctx.quadraticCurveTo(x + w, y, x + w, y + r);
  ctx.lineTo(x + w, y + h - r);
  ctx.quadraticCurveTo(x + w, y + h, x + w - r, y + h);
  ctx.lineTo(x + r, y + h);
  ctx.quadraticCurveTo(x, y + h, x, y + h - r);
  ctx.lineTo(x, y + r);
  ctx.quadraticCurveTo(x, y, x + r, y);
  ctx.closePath();
  ctx.fill();
}

/// Test hook: reset state between scenarios.
export function resetWaveformForTests() {
  levels = new Float32Array(BARS);
  targets = new Float32Array(BARS);
  head = 0;
  lastSampleAt = 0;
}
