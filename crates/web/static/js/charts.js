// Minimal zero-dependency canvas sparklines for the status page (SPEC §3.2).
// Each chart keeps a bounded rolling window; push() re-renders. Real-time
// only — no history beyond the window is kept anywhere.

const WINDOW = 60; // points kept per chart (~2 min at 2s polling)

const charts = new Map(); // id -> {points: [], max: null, two: [[], []]}

function cssVar(name, fallback) {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

function setupCanvas(canvas) {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth || 200;
  const h = canvas.clientHeight || 56;
  if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
  }
  const ctx = canvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  return { ctx, w, h };
}

function drawSeries(ctx, w, h, points, max, color) {
  if (points.length < 2) return;
  const top = max || Math.max(...points, 1e-9);
  const step = w / (WINDOW - 1);
  const x0 = w - (points.length - 1) * step; // anchor right — newest at the edge
  ctx.beginPath();
  points.forEach((v, i) => {
    const x = x0 + i * step;
    const y = h - 3 - (Math.min(v, top) / top) * (h - 6);
    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  });
  ctx.strokeStyle = color;
  ctx.lineWidth = 1.6;
  ctx.lineJoin = 'round';
  ctx.stroke();
  // soft fill under the line
  const last = points[points.length - 1];
  const lastY = h - 3 - (Math.min(last, top) / top) * (h - 6);
  ctx.lineTo(w, h);
  ctx.lineTo(x0, h);
  ctx.closePath();
  ctx.globalAlpha = 0.12;
  ctx.fillStyle = color;
  ctx.fill();
  ctx.globalAlpha = 1;
}

export function initChart(id, opts = {}) {
  charts.set(id, { points: [], two: opts.two ? [[], []] : null, fmt: opts.fmt });
}

export function pushChart(id, value) {
  const c = charts.get(id);
  if (!c) return;
  c.points.push(value);
  if (c.points.length > WINDOW) c.points.shift();
  const canvas = document.getElementById(id);
  if (!canvas) return;
  const { ctx, w, h } = setupCanvas(canvas);
  const accent = cssVar('--accent-grad', '').match(/#[0-9a-fA-F]+/) || [];
  const color = cssVar('--obs-line', accent[0] || '#00c8a0');
  drawSeries(ctx, w, h, c.points, 0, color);
  if (c.fmt) {
    const out = document.getElementById(id + '-val');
    if (out) out.textContent = c.fmt(value);
  }
}

// Two-series chart (e.g. rx/tx rates) sharing one canvas.
export function pushChartTwo(id, v1, v2) {
  const c = charts.get(id);
  if (!c || !c.two) return;
  c.two[0].push(v1);
  c.two[1].push(v2);
  if (c.two[0].length > WINDOW) c.two[0].shift();
  if (c.two[1].length > WINDOW) c.two[1].shift();
  const canvas = document.getElementById(id);
  if (!canvas) return;
  const { ctx, w, h } = setupCanvas(canvas);
  drawSeries(ctx, w, h, c.two[0], 0, cssVar('--obs-line-rx', '#21b8d8'));
  drawSeries(ctx, w, h, c.two[1], 0, cssVar('--obs-line-tx', '#00c8a0'));
  const out = document.getElementById(id + '-val');
  if (out) out.textContent = (c.fmt ? c.fmt(v1) : v1) + ' / ' + (c.fmt ? c.fmt(v2) : v2);
}

export function fmtBytes(n) {
  const v = Number(n) || 0;
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let x = v;
  while (x >= 1024 && i < units.length - 1) { x /= 1024; i += 1; }
  return (i === 0 ? x : x.toFixed(1)) + ' ' + units[i];
}

export function fmtRate(n) {
  return fmtBytes(n) + '/s';
}

export function fmtPercent(n) {
  return (Number(n) || 0).toFixed(1) + '%';
}
