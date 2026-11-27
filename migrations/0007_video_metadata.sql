-- Video and animation metadata.
--
-- `width`/`height` keep the frame dimensions (after container rotation).
-- These columns add the temporal information the client sorts and displays.
-- Existing rows stay NULL, which means "not probed yet"; the next sync fills
-- them in because the indexer now probes every file it has to read.

ALTER TABLE media ADD COLUMN framerate REAL
    CHECK (framerate IS NULL OR framerate >= 0);

ALTER TABLE media ADD COLUMN frame_count INTEGER
    CHECK (frame_count IS NULL OR frame_count >= 0);

CREATE INDEX idx_media_album_framerate ON media (album_id, framerate);
