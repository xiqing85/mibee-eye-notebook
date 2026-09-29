// Hearing records view (extension: audio_records, SPEC appendix A #24)
// + voiceprint speakers card (SPEC appendix A #25).
// Persistent text records of what the device heard — sound-event classes
// and voice-interaction transcripts. Newest first; optional kind filter;
// clear-all with confirmation. Live refresh rides the existing SSE events
// (alarm source:"audio" / voice_transcript) while the view is open.
// The speakers card enrolls wake-word voiceprints: POST arms the engine,
// the user says the wake word N times, GET polls progress, commit saves.

import { api } from './api.js';
import { hasCap } from './store.js';
import { $, el, confirmDlg, toast } from './ui.js';
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
    const speakerBadge = (isVoice && rec.speaker)
      ? el('span', { className: 'record-kind kind-speaker', textContent: rec.speaker, title: t('recordSpeaker') })
      : el('span');
    list.appendChild(el('div', { className: 'record-row' }, [
      el('span', { className: 'record-time mono', textContent: fmtTime(rec.timestamp_ms) }),
      el('span', { className: 'record-kind ' + (isVoice ? 'kind-voice' : 'kind-sound'), textContent: isVoice ? t('recordsKindVoice') : t('recordsKindSound') }),
      speakerBadge,
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
  updateSpeakersVisibility();
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
  initSpeakers();
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

// -- Voiceprint speakers card (SPEC appendix A #25) ----------------------

let enrollPolling = false;

function fmtSpeakerProgress(collected, needed) {
  return t('speakersProgress').replace('{collected}', String(collected)).replace('{needed}', String(needed));
}

export async function renderSpeakers() {
  const card = $('speakers-card');
  if (!card || card.classList.contains('hidden')) return;
  const listEl = $('speakers-list');
  if (!listEl) return;
  const r = await api.get('/api/voice/speakers');
  if (!r.ok) return;
  const speakers = r.data && r.data.speakers ? r.data.speakers : [];
  listEl.innerHTML = '';
  if (speakers.length === 0) {
    listEl.appendChild(el('div', { className: 'record-empty', textContent: t('speakersEmpty') }));
    return;
  }
  for (const sp of speakers) {
    const chip = el('span', { className: 'speaker-chip' }, [
      el('span', { className: 'speaker-name', textContent: sp.name }),
      el('span', { className: 'speaker-samples mono', textContent: '×' + sp.count }),
      el('button', {
        type: 'button', className: 'speaker-del', textContent: '✕',
        title: t('speakersDeleteConfirm').replace('{name}', sp.name),
        onclick: async () => {
          const ok = await confirmDlg({ message: t('speakersDeleteConfirm').replace('{name}', sp.name), danger: true });
          if (!ok) return;
          await api.del('/api/voice/speakers/' + encodeURIComponent(sp.name));
          renderSpeakers();
        },
      }),
    ]);
    listEl.appendChild(chip);
  }
}

function setEnrollUi(active, collected, needed) {
  const prog = $('speaker-progress');
  const cancel = $('speaker-cancel');
  if (prog) {
    prog.classList.toggle('hidden', !active);
    if (active) prog.textContent = fmtSpeakerProgress(collected, needed);
  }
  if (cancel) cancel.classList.toggle('hidden', !active);
  const enroll = $('speaker-enroll');
  if (enroll) enroll.disabled = active;
}

async function pollEnrollment() {
  if (enrollPolling) return;
  enrollPolling = true;
  try {
    for (;;) {
      const r = await api.get('/api/voice/speakers');
      if (!r.ok) break;
      const st = r.data && r.data.enrollment;
      if (!st) break; // committed or cancelled elsewhere
      setEnrollUi(true, st.collected, st.needed);
      if (st.collected >= st.needed) {
        const c = await api.post('/api/voice/speakers/commit', {});
        if (c.ok) toast(t('speakersEnrolled'), 'success');
        else toast(t('speakersCommitFailed'), 'error');
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 1500));
    }
  } finally {
    enrollPolling = false;
    setEnrollUi(false, 0, 0);
    renderSpeakers();
  }
}

async function startEnroll() {
  const input = $('speaker-name');
  if (!input) return;
  const name = input.value.trim();
  if (!name) {
    input.focus();
    return;
  }
  const r = await api.post('/api/voice/speakers', { name, utterances: 3 });
  if (!r.ok) {
    toast(t('speakersEnrollFailed'), 'error');
    return;
  }
  input.value = '';
  pollEnrollment();
}

function initSpeakers() {
  const enroll = $('speaker-enroll');
  if (enroll) enroll.addEventListener('click', startEnroll);
  const cancel = $('speaker-cancel');
  if (cancel) {
    cancel.addEventListener('click', async () => {
      await api.post('/api/voice/speakers/cancel', {});
      setEnrollUi(false, 0, 0);
    });
  }
  const input = $('speaker-name');
  if (input) {
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') startEnroll();
    });
  }
}

function updateSpeakersVisibility() {
  const card = $('speakers-card');
  if (card) card.classList.toggle('hidden', !hasCap('voice_speakers'));
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
