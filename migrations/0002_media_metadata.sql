-- Album indexing timestamp + uniqueness needed by GET /api/album/<hash>.
ALTER TABLE albums ADD COLUMN indexed_at TEXT;

DROP INDEX IF EXISTS idx_albums_hash;
CREATE UNIQUE INDEX idx_albums_hash_unique ON albums (hash);

-- Media metadata used by /api/album/<hash>/media sorting.
ALTER TABLE media ADD COLUMN date TEXT;
ALTER TABLE media ADD COLUMN size INTEGER NOT NULL DEFAULT 0 CHECK (size >= 0);
-- Natural-sort key: name with every digit run zero-padded ("img2" -> "img0000000002").
ALTER TABLE media ADD COLUMN sort_name TEXT NOT NULL DEFAULT '';

DROP INDEX IF EXISTS idx_media_hash;
CREATE UNIQUE INDEX idx_media_album_hash ON media (album_id, hash);

CREATE INDEX idx_media_album_date ON media (album_id, date);
CREATE INDEX idx_media_album_sort_name ON media (album_id, sort_name);
CREATE INDEX idx_media_album_size ON media (album_id, size);

-- Backfill for rows created before this migration (pure SQL cannot pad digits).
UPDATE media SET sort_name = lower(name) WHERE sort_name = '';
