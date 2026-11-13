-- SVG files are a separate media type now: they scale losslessly and are
-- counted as images by clients, but are neither raster stills nor animations.
--
-- SQLite cannot alter a CHECK constraint in place, so the table is rebuilt.
-- Ids, hashes and timestamps are copied verbatim; the indexes and the
-- `updated_at` trigger are recreated below.
CREATE TABLE media_new (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT    NOT NULL,
    hash         TEXT    NOT NULL,
    type         TEXT    NOT NULL CHECK (type IN ('image', 'video', 'audio', 'animated', 'vector')),
    album_id     INTEGER NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    width        INTEGER NOT NULL DEFAULT 0 CHECK (width >= 0),
    height       INTEGER NOT NULL DEFAULT 0 CHECK (height >= 0),
    duration_sec REAL             CHECK (duration_sec IS NULL OR duration_sec >= 0),
    date         TEXT,
    size         INTEGER NOT NULL DEFAULT 0 CHECK (size >= 0),
    sort_name    TEXT    NOT NULL DEFAULT '',
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

INSERT INTO media_new (
    id, name, hash, type, album_id, width, height,
    duration_sec, date, size, sort_name, created_at, updated_at
)
SELECT
    id, name, hash,
    CASE type WHEN 'svg' THEN 'vector' ELSE type END,
    album_id, width, height,
    duration_sec, date, size, sort_name, created_at, updated_at
FROM media;

DROP TABLE media;
ALTER TABLE media_new RENAME TO media;

CREATE INDEX idx_media_album_id ON media (album_id);
CREATE INDEX idx_media_type ON media (type);
CREATE UNIQUE INDEX idx_media_album_hash ON media (album_id, hash);
CREATE INDEX idx_media_album_date ON media (album_id, date);
CREATE INDEX idx_media_album_sort_name ON media (album_id, sort_name);
CREATE INDEX idx_media_album_size ON media (album_id, size);
CREATE UNIQUE INDEX idx_media_album_name ON media (album_id, name);

CREATE TRIGGER media_touch_updated_at
AFTER UPDATE ON media
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE media
       SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
     WHERE id = NEW.id;
END;
