-- Keep `album_stats` in sync with the album tree and media rows.
--
-- Only direct counters are materialized. Every trigger is O(1); no trigger
-- walks ancestors, so bulk indexing and cascading deletes stay predictable.

-- A new album gets an empty stats row, and its parent's child count grows.
CREATE TRIGGER albums_stats_after_insert
AFTER INSERT ON albums
FOR EACH ROW
BEGIN
    INSERT INTO album_stats (album_id) VALUES (NEW.id);

    UPDATE album_stats
       SET albums_count = albums_count + 1
     WHERE album_id = NEW.parent_id;
END;

-- Deleting an album also deletes its stats row and its media via foreign keys.
CREATE TRIGGER albums_stats_after_delete
AFTER DELETE ON albums
FOR EACH ROW
BEGIN
    UPDATE album_stats
       SET albums_count = albums_count - 1
     WHERE album_id = OLD.parent_id;
END;

-- Re-parenting is not exposed by the API yet, but keep the counters correct
-- if it ever happens (direct SQL, import, future move feature).
CREATE TRIGGER albums_stats_after_parent_update
AFTER UPDATE OF parent_id ON albums
FOR EACH ROW
WHEN OLD.parent_id IS NOT NEW.parent_id
BEGIN
    UPDATE album_stats
       SET albums_count = albums_count - 1
     WHERE album_id = OLD.parent_id;

    UPDATE album_stats
       SET albums_count = albums_count + 1
     WHERE album_id = NEW.parent_id;
END;

-- Media insert: add one item to the owning album's direct counters.
CREATE TRIGGER media_stats_after_insert
AFTER INSERT ON media
FOR EACH ROW
BEGIN
    UPDATE album_stats
       SET medias_count = medias_count + 1,
           images_count = images_count + (NEW.type IN ('image', 'vector')),
           videos_count = videos_count + (NEW.type IN ('video', 'animated')),
           audios_count = audios_count + (NEW.type = 'audio'),
           size         = size + NEW.size,
           duration     = duration + COALESCE(NEW.duration_sec, 0)
     WHERE album_id = NEW.album_id;
END;

-- Media delete: subtract the removed row. This also fires for rows removed by
-- ON DELETE CASCADE; if the album's stats row is already gone the UPDATE is a
-- no-op, which is fine.
CREATE TRIGGER media_stats_after_delete
AFTER DELETE ON media
FOR EACH ROW
BEGIN
    UPDATE album_stats
       SET medias_count = medias_count - 1,
           images_count = images_count - (OLD.type IN ('image', 'vector')),
           videos_count = videos_count - (OLD.type IN ('video', 'animated')),
           audios_count = audios_count - (OLD.type = 'audio'),
           size         = size - OLD.size,
           duration     = duration - COALESCE(OLD.duration_sec, 0)
     WHERE album_id = OLD.album_id;
END;

-- Media update: move the delta between albums and/or swap the counters when
-- meaningful fields change. The guard skips the `updated_at` touch trigger and
-- no-op upserts from the indexer.
CREATE TRIGGER media_stats_after_update
AFTER UPDATE OF album_id, type, size, duration_sec ON media
FOR EACH ROW
WHEN OLD.album_id <> NEW.album_id
  OR OLD.type <> NEW.type
  OR OLD.size <> NEW.size
  OR OLD.duration_sec IS NOT NEW.duration_sec
BEGIN
    UPDATE album_stats
       SET medias_count = medias_count - 1,
           images_count = images_count - (OLD.type IN ('image', 'vector')),
           videos_count = videos_count - (OLD.type IN ('video', 'animated')),
           audios_count = audios_count - (OLD.type = 'audio'),
           size         = size - OLD.size,
           duration     = duration - COALESCE(OLD.duration_sec, 0)
     WHERE album_id = OLD.album_id;

    UPDATE album_stats
       SET medias_count = medias_count + 1,
           images_count = images_count + (NEW.type IN ('image', 'vector')),
           videos_count = videos_count + (NEW.type IN ('video', 'animated')),
           audios_count = audios_count + (NEW.type = 'audio'),
           size         = size + NEW.size,
           duration     = duration + COALESCE(NEW.duration_sec, 0)
     WHERE album_id = NEW.album_id;
END;
