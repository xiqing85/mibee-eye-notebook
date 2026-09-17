// SPEC §6 `alarm` events: rising-edge alarms accepted on the edge — same
// source and gating as the GB28181 Alarm NOTIFY, but pushed regardless of
// platform delivery. Payload: { camera_id, active, source, targets,
// timestamp }; `active` is always true on the wire (rising edge), anything
// else is ignored. Devices only advertise the event when it can fire, so
// no capability gating is needed here.

import { toast } from './ui.js';
import { t } from './i18n.js';

// The edge already applies a cooldown (default 30s); this only collapses
// duplicate toasts if a device ever bursts two identical edges quickly.
const DEDUPE_MS = 2500;
let lastToast = null;

export function handleAlarmEvent(p) {
  if (!p || p.active === false) return; // rising-edge events only
  const targets = Math.max(0, Number(p.targets) || 0);
  const msg = t('alarmTriggered', { n: targets });
  const now = Date.now();
  if (lastToast && lastToast.msg === msg && now - lastToast.at < DEDUPE_MS) return;
  lastToast = { msg, at: now };
  toast(msg, 'error');
}
