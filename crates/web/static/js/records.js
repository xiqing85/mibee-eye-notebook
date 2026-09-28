// Hearing records view (extension: audio_records, SPEC appendix A #24).
// Persistent text records of what the device heard — sound-event classes
// and voice-interaction transcripts. Newest first; optional kind filter;
// clear-all with confirmation. Live refresh rides the existing SSE events
// (alarm source:"audio" / voice_transcript) while the view is open.

import { api } from './api.js';
import { hasCap } from './store.js';
import { $, el, confirmDlg } from './ui.js';
import { t } from './i18n.js';

let currentKind = '';

function fmtTime(ms) {
  const d = new Date(ms);
  const pad = (n) => String(n).padStart(2, '0');
  return d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate()) +
    ' ' + pad(d.getHours()) + ':' + pad(d.getMinutes()) + ':' + pad(d.getSeconds());
}

export async function renderRecords() {
  const view = $('view-records');
  if (!view) return;
  const list = $('records-list');
  const params = new URLSearchParams();
  if (currentKind) params.set('kind', currentKind);
  params.set('limit', '200');
  const r = await api.get('/api/audio/records?' + params.toString());
  list.innerHTML = '';
  if (!r.ok) {
    list.appendChild(el('div', { className: 'record-empty', textContent: t('recordsLoadFailed') }));
    return;
  }
  const records = r.data && r.data.records ? r.data.records : [];
  if (records.length === 0) {
    list.appendChild(el('div', { className: 'record-empty', textContent: t('recordsEmpty') }));
    return;
  }
  for (const rec of records) {
    const isVoice = rec.kind === 'voice';
    const textCell = el('span', { className: 'record-text' }, [document.createTextNode(rec.text)]);
    if (isVoice && rec.keyword) {
      textCell.appendChild(el('span', { className: 'record-keyword', textContent: '«' + rec.keyword + '»' }));
    }
    list.appendChild(el('div', { className: 'record-row' }, [
      el('span', { className: 'record-time mono', textContent: fmtTime(rec.timestamp_ms) }),
      el('span', { className: 'record-kind ' + (isVoice ? 'kind-voice' : 'kind-sound'), textContent: isVoice ? t('recordsKindVoice') : t('recordsKindSound') }),
      textCell,
      typeof rec.score === 'number'
        ? el('span', { className: 'record-score mono', textContent: rec.score.toFixed(2) })
        : el('span'),
    ]));
  }
}

export function updateRecordsVisibility() {
  const show = hasCap('audio_records');
  document.querySelectorAll('.nav-tab[data-view="records"]').forEach((tab) => {
    tab.classList.toggle('hidden', !show);
  });
}

export function initRecords() {
  const filter = $('records-kind');
  if (filter) {
    filter.addEventListener('change', () => {
      currentKind = filter.value;
      renderRecords();
    });
  }
  const refresh = $('records-refresh');
  if (refresh) refresh.addEventListener('click', renderRecords);
  const clear = $('records-clear');
  if (clear) {
    clear.addEventListener('click', async () => {
      const ok = await confirmDlg({ message: t('recordsClearConfirm'), danger: true });
      if (!ok) return;
      const r = await api.del('/api/audio/records');
      if (r.ok) renderRecords();
    });
  }
  updateRecordsVisibility();
}

// Live refresh while the records view is open: sound alarms and voice
// transcripts both mean a new record landed.
export function recordsSseHook(eventName, payload) {
  const relevant = (eventName === 'alarm' && payload && payload.source === 'audio') ||
    eventName === 'voice_transcript';
  if (!relevant) return;
  const view = $('view-records');
  if (view && view.classList.contains('active')) renderRecords();
}
