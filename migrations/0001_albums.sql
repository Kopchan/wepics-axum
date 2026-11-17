-- Albums: folders with optional nesting via parent_id.

CREATE TABLE albums (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL,
    -- Opaque URL identifier: random UUID v4 encoded in Base62 (22 chars),
    -- or the literal `root` for the top-level album. See helpers::hashes.
    hash       TEXT    NOT NULL,
    parent_id  INTEGER REFERENCES albums (id) ON DELETE CASCADE,
    -- Directory relative to the media root, `/`-separated and empty for the
    -- root album. Stable across hash changes and album renames.
    path       TEXT    NOT NULL DEFAULT '',
    -- Canonical URL name. When set, the album can be addressed by it instead
    -- of its hash. Partial index below keeps aliases unique.
    alias      TEXT,
    -- JSON array of former aliases, kept so old links can be shown in the
    -- edit form. Only the current `alias` is used for routing.
    old_aliases TEXT   NOT NULL DEFAULT '[]',
    -- Age-rating level, matching the client's rating presets.
    rating_level INTEGER,
    -- Manual position for `asort=order`; 0 means "not set".
    order_level INTEGER NOT NULL DEFAULT 0,
    -- `NULL` inherits the parent's visibility, `0` hides, `1` allows guests.
    guest_allow INTEGER CHECK (guest_allow IS NULL OR guest_allow IN (0, 1)),
    -- Set after a successful scan, so a failed sync is retried next time.
    indexed_at TEXT,
    -- Natural-sort key: name with every digit run zero-padded
    -- ("album2" -> "album0000000002"). Writers must supply
    -- models::natural_sort_key; pure SQL cannot pad digit runs.
    sort_name  TEXT    NOT NULL DEFAULT '',
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    CHECK (parent_id IS NULL OR parent_id <> id)
);

CREATE UNIQUE INDEX idx_albums_hash_unique ON albums (hash);
CREATE UNIQUE INDEX idx_albums_path_unique ON albums (path);
CREATE UNIQUE INDEX idx_albums_alias_unique ON albums (alias) WHERE alias IS NOT NULL;
CREATE INDEX idx_albums_parent_id ON albums (parent_id);
CREATE INDEX idx_albums_parent_sort_name ON albums (parent_id, sort_name);
CREATE INDEX idx_albums_parent_created_at ON albums (parent_id, created_at);
