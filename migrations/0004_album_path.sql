-- Directory of an album relative to the media root, `/`-separated and empty
-- for the root album. Albums discovered on disk need a stable folder path that
-- is independent of their (random) URL hash.
ALTER TABLE albums ADD COLUMN path TEXT NOT NULL DEFAULT '';

-- Preserve the old layout for rows created before this migration: their folder
-- was `<media>/<hash>`.
UPDATE albums SET path = hash WHERE hash <> 'root';

CREATE UNIQUE INDEX idx_albums_path_unique ON albums (path);
