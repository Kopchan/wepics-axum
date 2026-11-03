-- Albums: folders with optional nesting via parent_id.
CREATE TABLE albums (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL,
    hash       TEXT    NOT NULL,
    parent_id  INTEGER REFERENCES albums (id) ON DELETE CASCADE,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    CHECK (parent_id IS NULL OR parent_id <> id)
);

CREATE INDEX idx_albums_parent_id ON albums (parent_id);
CREATE INDEX idx_albums_hash ON albums (hash);

-- Media: files belonging to an album.
CREATE TABLE media (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT    NOT NULL,
    hash         TEXT    NOT NULL,
    type         TEXT    NOT NULL CHECK (type IN ('image', 'video', 'audio', 'animated')),
    album_id     INTEGER NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    width        INTEGER NOT NULL DEFAULT 0 CHECK (width >= 0),
    height       INTEGER NOT NULL DEFAULT 0 CHECK (height >= 0),
    duration_sec REAL             CHECK (duration_sec IS NULL OR duration_sec >= 0),
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_media_album_id ON media (album_id);
CREATE INDEX idx_media_hash ON media (hash);
CREATE INDEX idx_media_type ON media (type);

-- Keep updated_at honest even for raw UPDATEs.
-- The WHEN guard stops the trigger from re-firing on its own UPDATE.
CREATE TRIGGER albums_touch_updated_at
AFTER UPDATE ON albums
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE albums
       SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
     WHERE id = NEW.id;
END;

CREATE TRIGGER media_touch_updated_at
AFTER UPDATE ON media
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE media
       SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
     WHERE id = NEW.id;
END;
