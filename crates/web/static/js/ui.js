// DOM + toast + dialog helpers shared by every view module.

import { icon } from './icons.js';

export const $ = (id) => document.getElementById(id);

export function esc(s) {
  return String(s).replace(/[&<>"]/g, (c) => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]
  ));
}

/// Create an element: el('div', {className:'x', textContent:'y', html:'<b>'}, [children])
export function el(tag, attrs = {}, children = []) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === undefined || v === null) continue;
    if (k === 'className') node.className = v;
    else if (k === 'textContent') node.textContent = v;
    else if (k === 'html') node.innerHTML = v;
    else if (k === 'dataset') Object.assign(node.dataset, v);
    else if (k.startsWith('on') && typeof v === 'function') node.addEventListener(k.slice(2), v);
    else node.setAttribute(k, v);
  }
  for (const c of children) if (c) node.appendChild(c);
  return node;
}

const TOAST_ICONS = { success: 'check', error: 'alert', info: 'info' };

let toastSeq = 0;
export function toast(msg, type = 'info') {
  const container = $('toast-container');
  if (!container) return;
  // Dedupe consecutive identical toasts.
  const last = container.lastElementChild;
  if (last && last.dataset.msg === msg && last.dataset.type === type) return;
  const item = el('div', {
    className: 'toast toast-' + type,
    dataset: { msg, type },
  }, [
    el('span', { className: 'toast-ico', html: icon(TOAST_ICONS[type] || 'info', 16) }),
    el('span', { className: 'toast-msg', textContent: msg }),
    el('button', {
      className: 'toast-close', 'aria-label': '×', html: icon('x', 13),
      onclick: () => dismissToast(item),
    }),
  ]);
  container.appendChild(item);
  while (container.children.length > 3) container.firstElementChild.remove();
  const mySeq = ++toastSeq;
  item._timer = setTimeout(() => dismissToast(item), 3500);
  item._seq = mySeq;
}

function dismissToast(item) {
  if (!item || !item.isConnected) return;
  clearTimeout(item._timer);
  item.classList.add('toast-out');
  setTimeout(() => item.remove(), 350);
}

/// In-app confirmation dialog (replaces window.confirm). All labels are
/// pre-rendered by the caller so this module stays i18n-free.
export function confirmDlg({ message, okText = 'OK', cancelText = 'Cancel', danger = false }) {
  return new Promise((resolve) => {
    const overlay = $('confirm-overlay');
    if (!overlay) { resolve(window.confirm(message)); return; }
    $('confirm-msg').textContent = message;
    const ok = $('confirm-ok'), cancel = $('confirm-cancel');
    ok.textContent = okText;
    cancel.textContent = cancelText;
    ok.className = danger ? 'btn-primary btn-danger-solid' : 'btn-primary';
    const ico = $('confirm-icon');
    ico.className = 'confirm-icon' + (danger ? ' danger' : '');
    ico.innerHTML = icon(danger ? 'trash' : 'alert', 24);
    overlay.classList.remove('hidden');

    const done = (val) => {
      overlay.classList.add('hidden');
      document.removeEventListener('keydown', onKey);
      ok.onclick = cancel.onclick = overlay.onclick = null;
      resolve(val);
    };
    const onKey = (e) => { if (e.key === 'Escape') done(false); };
    ok.onclick = () => done(true);
    cancel.onclick = () => done(false);
    overlay.onclick = (e) => { if (e.target === overlay) done(false); };
    document.addEventListener('keydown', onKey);
    ok.focus();
  });
}

/// Toggle a button's inline loading spinner (label stays, hidden by CSS).
export function setBtnLoading(btn, loading) {
  if (!btn) return;
  btn.classList.toggle('is-loading', !!loading);
  btn.disabled = !!loading;
}

export function badge(elId, labelId, cls, label) {
  const b = $(elId), l = $(labelId);
  if (b) b.className = 'status-badge ' + cls;
  if (l) l.textContent = label;
}
