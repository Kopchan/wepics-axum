-- Media are addressed by their file name: a folder may contain `photo.jpg`
-- and `photo.png`, which share a stem and can both be indexed. Identity within
-- an album is therefore the full file name, not the stem used as a URL hash.
CREATE UNIQUE INDEX idx_media_album_name ON media (album_id, name);
