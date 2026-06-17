-- Add offline_since column to track when a camera went offline (hot-plug removal).
--
-- NULL when the camera is online/available.
-- Set to datetime('now') when udev detects the physical device was removed.
-- Reset to NULL when the device is plugged back in and re-discovered.

ALTER TABLE cameras ADD COLUMN offline_since TEXT;
