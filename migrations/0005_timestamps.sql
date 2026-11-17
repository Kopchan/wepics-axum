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
