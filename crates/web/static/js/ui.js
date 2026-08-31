// DOM + toast helpers shared by every view module.

export const $ = (id) => document.getElementById(id);

export function esc(s) {
  return String(s).replace(/[&<>"]/g, (c) => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]
  ));
}

/// Create an element: el('div', {className:'x', textContent:'y', ...attrs}, [children])
export function el(tag, attrs = {}, children = []) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === undefined || v === null) continue;
    if (k === 'textContent') node.textContent = v;
    else if (k === 'dataset') Object.assign(node.dataset, v);
    else if (k.startsWith('on') && typeof v === 'function') node.addEventListener(k.slice(2), v);
    else node.setAttribute(k, v);
  }
  for (const c of children) if (c) node.appendChild(c);
  return node;
}

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
    el('span', { className: 'toast-msg', textContent: msg }),
    el('button', {
      className: 'toast-close', 'aria-label': '×', textContent: '×',
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

export function badge(elId, labelId, cls, label) {
  const b = $(elId), l = $(labelId);
  if (b) b.className = 'status-badge ' + cls;
  if (l) l.textContent = label;
}
