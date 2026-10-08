// Tools & skills card (SPEC §3.5): the registry the agent can call —
// built-ins plus MCP plugin servers. The list is the transparency
// surface required by the tool framework ("what is exposed to the
// model"), refreshed on view entry and every 60s while visible.

import { api } from './api.js';
import { hasCap } from './store.js';
import { $ } from './ui.js';
import { t } from './i18n.js';

let timer = null;

export function toolsCap() {
  return !!hasCap('tools');
}

export function initTools() {
  const card = $('tools-card');
  if (card) card.classList.toggle('hidden', !toolsCap());
  const btn = $('tools-refresh');
  if (btn) btn.addEventListener('click', () => refreshTools());
}

export async function refreshTools() {
  if (!toolsCap()) return;
  const r = await api.get('/api/tools');
  if (!r.ok || !r.data || !Array.isArray(r.data.tools)) return;
  const box = $('tools-list');
  if (!box) return;
  box.innerHTML = '';
  if (!r.data.tools.length) {
    const empty = document.createElement('p');
    empty.className = 'record-empty';
    empty.textContent = t('toolsEmpty');
    box.appendChild(empty);
    return;
  }
  for (const tool of r.data.tools) {
    const row = document.createElement('div');
    row.className = 'tool-row';
    const head = document.createElement('div');
    head.className = 'tool-row-head';
    const name = document.createElement('span');
    name.className = 'tool-row-name mono';
    name.textContent = String(tool.name || '?');
    head.appendChild(name);
    const src = document.createElement('span');
    src.className = 'tool-row-source';
    if (typeof tool.source === 'string' && tool.source.startsWith('mcp:')) {
      src.classList.add('mcp');
      src.textContent = t('toolsSourceMcp', { name: tool.source.slice(4) });
    } else {
      src.classList.add('builtin');
      src.textContent = t('toolsSourceBuiltin');
    }
    head.appendChild(src);
    row.appendChild(head);
    if (tool.description) {
      const desc = document.createElement('div');
      desc.className = 'tool-row-desc';
      desc.textContent = String(tool.description);
      row.appendChild(desc);
    }
    box.appendChild(row);
  }
}

export function startToolsPolling() {
  if (timer) return;
  timer = setInterval(() => {
    if (!toolsCap()) return;
    const view = $('view-assistant');
    if (view && view.classList.contains('active')) refreshTools();
  }, 60000);
}
