-- Natural-sort key for albums, mirroring media.sort_name.
-- Writers must supply the zero-padded key (see models::natural_sort_key);
-- pure SQL cannot pad digit runs, so this backfill only lowercases.
ALTER TABLE albums ADD COLUMN sort_name TEXT NOT NULL DEFAULT '';

UPDATE albums SET sort_name = lower(name) WHERE sort_name = '';

CREATE INDEX idx_albums_parent_sort_name ON albums (parent_id, sort_name);
CREATE INDEX idx_albums_parent_created_at ON albums (parent_id, created_at);
