// Hearing records view (extension: audio_records, SPEC appendix A #24)
// + voiceprint speakers card (SPEC appendix A #25)
// + meeting minutes card (SPEC appendix A #27).
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
    // Correlated record (#30-C): what the camera saw when this was heard,
    // and the MP4 segment covering it when recording was on.
    const sceneBadge = rec.scene
      ? el('span', { className: 'record-kind kind-scene', textContent: '👁', title: rec.scene })
      : el('span');
    const mediaBadge = rec.media_ref
      ? el('span', { className: 'record-kind kind-media', textContent: '🎞', title: rec.media_ref })
      : el('span');
    list.appendChild(el('div', { className: 'record-row' }, [
      el('span', { className: 'record-time mono', textContent: fmtTime(rec.timestamp_ms) }),
      el('span', { className: 'record-kind ' + (isVoice ? 'kind-voice' : 'kind-sound'), textContent: isVoice ? t('recordsKindVoice') : t('recordsKindSound') }),
      speakerBadge,
      sceneBadge,
      mediaBadge,
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
  updateFacesVisibility();
  updateMeetingsVisibility();
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
  initFaces();
  initMeetings();
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

// -- Face recognition card (SPEC appendix A #33) --------------------------

let faceEnrollPolling = false;

function fmtFaceProgress(collected, needed) {
  return t('facesProgress').replace('{collected}', String(collected)).replace('{needed}', String(needed));
}

export async function renderFaces() {
  const card = $('faces-card');
  if (!card || card.classList.contains('hidden')) return;
  const listEl = $('faces-list');
  if (!listEl) return;
  const r = await api.get('/api/faces');
  if (!r.ok) return;
  const faces = r.data && r.data.faces ? r.data.faces : [];
  listEl.innerHTML = '';
  if (faces.length === 0) {
    listEl.appendChild(el('div', { className: 'record-empty', textContent: t('facesEmpty') }));
    return;
  }
  for (const f of faces) {
    listEl.appendChild(el('span', { className: 'speaker-chip' }, [
      el('span', { className: 'speaker-name', textContent: f.name }),
      el('button', {
        type: 'button', className: 'speaker-del', textContent: '✕',
        title: t('facesDeleteConfirm').replace('{name}', f.name),
        onclick: async () => {
          const ok = await confirmDlg({ message: t('facesDeleteConfirm').replace('{name}', f.name), danger: true });
          if (!ok) return;
          await api.del('/api/faces/' + encodeURIComponent(f.name));
          renderFaces();
        },
      }),
    ]));
  }
}

function setFaceEnrollUi(active, collected, needed) {
  const prog = $('face-progress');
  const cancel = $('face-cancel');
  if (prog) {
    prog.classList.toggle('hidden', !active);
    if (active) prog.textContent = fmtFaceProgress(collected, needed);
  }
  if (cancel) cancel.classList.toggle('hidden', !active);
  const enroll = $('face-enroll');
  if (enroll) enroll.disabled = active;
}

async function pollFaceEnrollment() {
  if (faceEnrollPolling) return;
  faceEnrollPolling = true;
  try {
    for (;;) {
      const r = await api.get('/api/faces');
      if (!r.ok) break;
      const st = r.data && r.data.enrollment;
      if (!st) break;
      setFaceEnrollUi(true, st.collected, st.needed);
      if (st.collected >= st.needed) {
        const c = await api.post('/api/faces/commit', {});
        if (c.ok) toast(t('facesEnrolled'), 'success');
        else toast(t('facesCommitFailed'), 'error');
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 1500));
    }
  } finally {
    faceEnrollPolling = false;
    setFaceEnrollUi(false, 0, 0);
    renderFaces();
  }
}

async function startFaceEnroll() {
  const input = $('face-name');
  if (!input) return;
  const name = input.value.trim();
  if (!name) {
    input.focus();
    return;
  }
  const r = await api.post('/api/faces', { name });
  if (!r.ok) {
    toast(r.message || t('facesEnrollFailed'), 'error');
    return;
  }
  input.value = '';
  toast(t('facesLookAtCamera'), 'info');
  pollFaceEnrollment();
}

function initFaces() {
  const enroll = $('face-enroll');
  if (enroll) enroll.addEventListener('click', startFaceEnroll);
  const cancel = $('face-cancel');
  if (cancel) {
    cancel.addEventListener('click', async () => {
      await api.post('/api/faces/cancel', {});
      renderFaces();
    });
  }
}

function updateFacesVisibility() {
  const card = $('faces-card');
  if (card) card.classList.toggle('hidden', !hasCap('face'));
  renderFaces();
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
  if (eventName === 'meeting_state') {
    // Lifecycle transitions refresh the minutes list and the recording
    // indicator (restore on recording, clear on stop).
    setRecordingUi(
      payload && payload.status === 'recording' ? payload.meeting_id : null,
      payload ? payload.timestamp : undefined,
    );
    renderMeetings();
    return;
  }
  const relevant = (eventName === 'alarm' && payload && payload.source === 'audio') ||
    eventName === 'voice_transcript';
  if (!relevant) return;
  const view = $('view-records');
  if (view && view.classList.contains('active')) renderRecords();
}

// -- Meeting minutes card (SPEC appendix A #27) ---------------------------

let meetingTimer = null;
let meetingPoll = null;
const expandedMeetings = new Set();

function fmtDuration(ms) {
  const total = Math.max(0, Math.round(ms / 1000));
  const m = Math.floor(total / 60);
  const sec = total % 60;
  return m + ':' + String(sec).padStart(2, '0');
}

function statusLabel(status) {
  const map = {
    recording: 'meetingsStatusRecording',
    processing: 'meetingsStatusProcessing',
    done: 'meetingsStatusDone',
    failed: 'meetingsStatusFailed',
  };
  return t(map[status] || 'meetingsStatusProcessing');
}

function speakerLabel(seg) {
  if (seg.speaker) return seg.speaker;
  return t('meetingsSpeakerN').replace('{n}', String((seg.speaker_index || 0) + 1));
}

function setRecordingUi(meetingId, startedAtMs) {
  const rec = $('meeting-rec');
  const start = $('meeting-start');
  const stop = $('meeting-stop');
  const timer = $('meeting-rec-timer');
  if (!rec || !start || !stop) return;
  const active = meetingId !== null && meetingId !== undefined;
  rec.classList.toggle('hidden', !active);
  start.classList.toggle('hidden', active);
  stop.classList.toggle('hidden', !active);
  if (meetingTimer) { clearInterval(meetingTimer); meetingTimer = null; }
  if (active && timer) {
    const started = startedAtMs || Date.now();
    const tick = () => { timer.textContent = fmtDuration(Date.now() - started); };
    tick();
    meetingTimer = setInterval(tick, 1000);
  }
}

async function startMeeting() {
  const r = await api.post('/api/meetings/start', {});
  if (!r.ok) {
    toast(t('meetingsStartFailed'), 'error');
    return;
  }
  setRecordingUi(r.data.id, r.data.started_at_ms);
  renderMeetings();
}

async function stopMeeting() {
  const list = await api.get('/api/meetings');
  const running = list.ok && list.data && list.data.meetings
    ? list.data.meetings.find((m) => m.status === 'recording') : null;
  if (!running) {
    setRecordingUi(null);
    return;
  }
  const r = await api.post('/api/meetings/' + running.id + '/stop', {});
  if (!r.ok) {
    toast(t('meetingsStopFailed'), 'error');
    return;
  }
  setRecordingUi(null);
  renderMeetings();
}

async function toggleMinutes(id) {
  if (expandedMeetings.has(id)) expandedMeetings.delete(id);
  else expandedMeetings.add(id);
  renderMeetings();
}

async function renderMeetingDetail(id, container) {
  const r = await api.get('/api/meetings/' + id);
  container.innerHTML = '';
  if (!r.ok || !r.data || !r.data.segments || r.data.segments.length === 0) {
    container.appendChild(el('div', { className: 'record-empty', textContent: t('meetingsEmpty') }));
    return;
  }
  for (const seg of r.data.segments) {
    container.appendChild(el('div', { className: 'meeting-seg' }, [
      el('span', { className: 'record-kind kind-voice', textContent: speakerLabel(seg) }),
      el('span', { className: 'record-time mono', textContent: fmtDuration(seg.start_ms) + '–' + fmtDuration(seg.end_ms) }),
      el('span', { className: 'record-text', textContent: seg.text }),
    ]));
  }
}

export async function renderMeetings() {
  const card = $('meetings-card');
  if (!card || card.classList.contains('hidden')) return;
  const listEl = $('meetings-list');
  if (!listEl) return;
  const r = await api.get('/api/meetings');
  if (!r.ok) return;
  const meetings = r.data && r.data.meetings ? r.data.meetings : [];
  listEl.innerHTML = '';
  // Restore the recording indicator for a session started elsewhere.
  const running = meetings.find((m) => m.status === 'recording');
  if (running && $('meeting-stop') && $('meeting-stop').classList.contains('hidden')) {
    setRecordingUi(running.id, running.started_at_ms);
  }
  if (meetings.length === 0) {
    listEl.appendChild(el('div', { className: 'record-empty', textContent: t('meetingsEmpty') }));
    scheduleProcessingPoll(meetings);
    return;
  }
  for (const m of meetings) {
    const meta = [];
    if (typeof m.num_speakers === 'number' && m.num_speakers > 0) {
      meta.push(t('meetingsSpk').replace('{n}', String(m.num_speakers)));
    }
    if (typeof m.num_segments === 'number' && m.num_segments > 0) {
      meta.push(t('meetingsSegs').replace('{n}', String(m.num_segments)));
    }
    if (typeof m.duration_ms === 'number' && m.duration_ms > 0) {
      meta.push(t('meetingsMins').replace('{n}', String(Math.max(1, Math.round(m.duration_ms / 60000)))));
    }
    const isDone = m.status === 'done';
    const row = el('div', { className: 'meeting-row' }, [
      el('span', { className: 'record-time mono', textContent: fmtTime(m.started_at_ms) }),
      el('span', { className: 'record-kind kind-' + (m.status === 'failed' ? 'sound' : 'voice'), textContent: statusLabel(m.status) }),
      el('span', { className: 'meeting-meta', textContent: meta.join(' · ') }),
      m.error ? el('span', { className: 'record-score mono', textContent: m.error, title: m.error }) : el('span'),
      el('span', { className: 'meeting-actions' }, [
        isDone
          ? el('button', {
              type: 'button', className: 'btn-small',
              textContent: expandedMeetings.has(m.id) ? t('meetingsHide') : t('meetingsShow'),
              onclick: () => toggleMinutes(m.id),
            })
          : el('span'),
        el('button', {
          type: 'button', className: 'speaker-del', textContent: '✕',
          title: t('meetingsDeleteConfirm'),
          onclick: async () => {
            const ok = await confirmDlg({ message: t('meetingsDeleteConfirm'), danger: true });
            if (!ok) return;
            await api.del('/api/meetings/' + m.id);
            renderMeetings();
          },
        }),
      ]),
    ]);
    listEl.appendChild(row);
    if (isDone && expandedMeetings.has(m.id)) {
      const detail = el('div', { className: 'meeting-detail' });
      listEl.appendChild(detail);
      renderMeetingDetail(m.id, detail);
    }
  }
  scheduleProcessingPoll(meetings);
}

// Safety net for the async pipeline when SSE reconnects mid-processing:
// poll while any meeting is processing and this view is open.
function scheduleProcessingPoll(meetings) {
  if (meetingPoll) { clearInterval(meetingPoll); meetingPoll = null; }
  if (!meetings.some((m) => m.status === 'processing')) return;
  meetingPoll = setInterval(async () => {
    const view = $('view-records');
    if (!view || !view.classList.contains('active')) {
      clearInterval(meetingPoll);
      meetingPoll = null;
      return;
    }
    const r = await api.get('/api/meetings');
    if (!r.ok || !(r.data && r.data.meetings).some((m) => m.status === 'processing')) {
      clearInterval(meetingPoll);
      meetingPoll = null;
      renderMeetings();
    }
  }, 4000);
}

function initMeetings() {
  const start = $('meeting-start');
  if (start) start.addEventListener('click', startMeeting);
  const stop = $('meeting-stop');
  if (stop) stop.addEventListener('click', stopMeeting);
}

function updateMeetingsVisibility() {
  const card = $('meetings-card');
  if (card) card.classList.toggle('hidden', !hasCap('meeting'));
}
