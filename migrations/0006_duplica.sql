-- Extra copies of the same content inside one album.
--
-- `media` keeps exactly one row per content hash (see `idx_media_album_hash`),
-- so every further copy of that content becomes a `duplica` row that points at
-- the media row holding the file. `album_id` is denormalized on purpose: the
-- stats triggers below stay O(1) and keep working when the referenced media row
-- is removed by `ON DELETE CASCADE`.

CREATE TABLE duplica (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    media_id   INTEGER NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    album_id   INTEGER NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    name       TEXT    NOT NULL,
    -- JSON captured at scan time: hash, size, date, width, height and the hash
    -- algorithm version. Lets the indexer skip re-hashing unchanged copies.
    data       TEXT    NOT NULL DEFAULT '{}',
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_duplica_media_id ON duplica (media_id);
CREATE INDEX idx_duplica_album_id ON duplica (album_id);
-- A file name is unique inside an album, exactly like in `media`.
CREATE UNIQUE INDEX idx_duplica_album_name ON duplica (album_id, name);

-- Bumped whenever `helpers::hashes::media_hash` changes its input. Rows written
-- by older algorithms are re-hashed once instead of being trusted forever.
ALTER TABLE media ADD COLUMN hash_version INTEGER NOT NULL DEFAULT 0;

-- Duplicate counter for the album stats API.
ALTER TABLE album_stats ADD COLUMN duplica_count INTEGER NOT NULL DEFAULT 0;

UPDATE album_stats
   SET duplica_count = (
       SELECT COUNT(*) FROM duplica WHERE duplica.album_id = album_stats.album_id
   );

-- Keep `duplica_count` in sync with the `duplica` table.
CREATE TRIGGER duplica_stats_after_insert
AFTER INSERT ON duplica
FOR EACH ROW
BEGIN
    UPDATE album_stats
       SET duplica_count = duplica_count + 1
     WHERE album_id = NEW.album_id;
END;

CREATE TRIGGER duplica_stats_after_delete
AFTER DELETE ON duplica
FOR EACH ROW
BEGIN
    UPDATE album_stats
       SET duplica_count = duplica_count - 1
     WHERE album_id = OLD.album_id;
END;

-- Moving a duplicate between albums is not exposed by the API, but the counter
-- must not drift if it ever happens through raw SQL.
CREATE TRIGGER duplica_stats_after_album_update
AFTER UPDATE OF album_id ON duplica
FOR EACH ROW
WHEN OLD.album_id <> NEW.album_id
BEGIN
    UPDATE album_stats
       SET duplica_count = duplica_count - 1
     WHERE album_id = OLD.album_id;

    UPDATE album_stats
       SET duplica_count = duplica_count + 1
     WHERE album_id = NEW.album_id;
END;

-- Keep `updated_at` honest for raw UPDATEs, like the other tables.
CREATE TRIGGER duplica_touch_updated_at
AFTER UPDATE ON duplica
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE duplica
       SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
     WHERE id = NEW.id;
END;
