-- Media: files belonging to an album.

CREATE TABLE media (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT    NOT NULL,
    -- Opaque URL identifier: XXH3-64 of the file name and contents,
    -- URL-safe Base64 (11 chars). See helpers::hashes.
    hash         TEXT    NOT NULL,
    type         TEXT    NOT NULL CHECK (type IN ('image', 'video', 'audio', 'animated', 'vector')),
    album_id     INTEGER NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    width        INTEGER NOT NULL DEFAULT 0 CHECK (width >= 0),
    height       INTEGER NOT NULL DEFAULT 0 CHECK (height >= 0),
    duration_sec REAL             CHECK (duration_sec IS NULL OR duration_sec >= 0),
    date         TEXT,
    size         INTEGER NOT NULL DEFAULT 0 CHECK (size >= 0),
    -- Age-rating level inherited from the rating presets; NULL means unset.
    rating_level INTEGER,
    -- Natural-sort key, mirroring albums.sort_name.
    sort_name    TEXT    NOT NULL DEFAULT '',
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_media_album_id ON media (album_id);
CREATE INDEX idx_media_type ON media (type);
-- Media URLs are album-scoped, so the same hash may appear in another album.
CREATE UNIQUE INDEX idx_media_album_hash ON media (album_id, hash);
CREATE INDEX idx_media_album_date ON media (album_id, date);
CREATE INDEX idx_media_album_sort_name ON media (album_id, sort_name);
CREATE INDEX idx_media_album_size ON media (album_id, size);
-- `photo.jpg` and `photo.png` are distinct media; identity is the full name.
CREATE UNIQUE INDEX idx_media_album_name ON media (album_id, name);
