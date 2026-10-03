// SPEC §6 `alarm` events: rising-edge alarms accepted on the edge — same
// source and gating as the GB28181 Alarm NOTIFY, but pushed regardless of
// platform delivery. Payload: { camera_id, active, source, targets,
// timestamp }; `active` is always true on the wire (rising edge), anything
// else is ignored. Devices only advertise the event when it can fire, so
// no capability gating is needed here.
//
// Notebook dialect extensions (SPEC appendix A #21–#23): sound alarms
// carry source:"audio" + class; the follow-up `alarm_description` event
// carries a VLM sentence for the triggering frame; `voice_transcript`
// events surface the voice loop.

import { toast } from './ui.js';
import { t, soundText } from './i18n.js';

// The edge already applies a cooldown (default 30s); this only collapses
// duplicate toasts if a device ever bursts two identical edges quickly.
const DEDUPE_MS = 2500;
let lastToast = null;

export function handleAlarmEvent(p) {
  if (!p || p.active === false) return; // rising-edge events only
  let msg;
  if (p.source === 'audio' && p.class) {
    msg = t('alarmSound', { c: soundText(p.class) });
  } else {
    msg = t('alarmTriggered', { n: Math.max(0, Number(p.targets) || 0) });
  }
  const now = Date.now();
  if (lastToast && lastToast.msg === msg && now - lastToast.at < DEDUPE_MS) return;
  lastToast = { msg, at: now };
  toast(msg, 'error');
}

/// VLM description of an alarm frame (SPEC appendix A #23). Arrives
/// asynchronously after the alarm; devices only advertise the event when
/// the VLM engine is active.
export function handleAlarmDescription(p) {
  if (!p || !p.description) return;
  toast(t('alarmDescription', { d: p.description }), 'info');
}

/// Voice interaction transcript (SPEC appendix A #22).
export function handleVoiceTranscript(p) {
  if (!p || !p.transcript) return;
  toast(t('voiceHeard', { s: p.transcript }), 'info');
}
