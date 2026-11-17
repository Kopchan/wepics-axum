-- Materialized direct counters for every album.
--
-- `nested` counters are derived from these rows with a recursive album walk,
-- so only direct changes have to be maintained. The table is deliberately
-- separate from `albums`: updating stats must not bump `albums.updated_at`.
--
-- Rows are created by the AFTER INSERT trigger on `albums` (see
-- 0004_album_stats_triggers.sql) and kept up to date by media triggers.

CREATE TABLE album_stats (
    album_id     INTEGER PRIMARY KEY REFERENCES albums (id) ON DELETE CASCADE,
    medias_count INTEGER NOT NULL DEFAULT 0,
    images_count INTEGER NOT NULL DEFAULT 0, -- image + vector
    videos_count INTEGER NOT NULL DEFAULT 0, -- video + animated
    audios_count INTEGER NOT NULL DEFAULT 0,
    albums_count INTEGER NOT NULL DEFAULT 0, -- direct child albums
    size         INTEGER NOT NULL DEFAULT 0,
    duration     REAL    NOT NULL DEFAULT 0
);

-- Backfill for databases that already contain rows. Fresh databases get no
-- rows here; the album insert trigger creates an empty row for every album.
INSERT INTO album_stats (
    album_id, medias_count, images_count, videos_count, audios_count,
    albums_count, size, duration
)
SELECT
    albums.id,
    (SELECT COUNT(*) FROM media WHERE media.album_id = albums.id),
    (SELECT COUNT(*) FROM media WHERE media.album_id = albums.id
        AND media.type IN ('image', 'vector')),
    (SELECT COUNT(*) FROM media WHERE media.album_id = albums.id
        AND media.type IN ('video', 'animated')),
    (SELECT COUNT(*) FROM media WHERE media.album_id = albums.id
        AND media.type = 'audio'),
    (SELECT COUNT(*) FROM albums child WHERE child.parent_id = albums.id),
    (SELECT COALESCE(SUM(media.size), 0) FROM media WHERE media.album_id = albums.id),
    (SELECT COALESCE(SUM(media.duration_sec), 0.0) FROM media WHERE media.album_id = albums.id)
FROM albums;
