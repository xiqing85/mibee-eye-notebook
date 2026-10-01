-- Correlated records, video dimension (SPEC appendix A #30-C): the MP4
-- segment covering the event timestamp, when local recording is on.
-- Empty when recording is off / the stream is stopped / a rotation race.
ALTER TABLE hearing_records ADD COLUMN media_ref TEXT NOT NULL DEFAULT '';
