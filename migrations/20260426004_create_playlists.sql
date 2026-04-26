CREATE TABLE IF NOT EXISTS playlists (
    id CHAR(12) PRIMARY KEY,

    room_id CHAR(12) NOT NULL REFERENCES rooms(id) ON DELETE RESTRICT,
    creator_id CHAR(12) REFERENCES users(id) ON DELETE RESTRICT,

    name VARCHAR(255) NOT NULL DEFAULT '',

    parent_id CHAR(12) REFERENCES playlists(id) ON DELETE RESTRICT,

    position DOUBLE PRECISION NOT NULL,

    source_provider VARCHAR(64),
    source_config JSONB,
    provider_instance_name VARCHAR(64),

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    version INTEGER NOT NULL DEFAULT 0,

    CONSTRAINT playlists_parent_not_self
        CHECK (parent_id IS NULL OR parent_id != id),
    CONSTRAINT playlists_dynamic_source_consistent
        CHECK (
        (source_provider IS NOT NULL AND source_config IS NOT NULL)
        OR (source_provider IS NULL AND source_config IS NULL)
        ),
    CONSTRAINT playlists_id_room_unique UNIQUE (id, room_id)
);

CREATE INDEX IF NOT EXISTS idx_playlists_room ON playlists(room_id);
CREATE INDEX IF NOT EXISTS idx_playlists_parent ON playlists(parent_id, position, id);
CREATE INDEX IF NOT EXISTS idx_playlists_tree ON playlists(room_id, parent_id, position, id);
CREATE INDEX IF NOT EXISTS idx_playlists_creator ON playlists(creator_id);
CREATE INDEX IF NOT EXISTS idx_playlists_source_provider
    ON playlists(source_provider)
    WHERE source_provider IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_playlists_created_at ON playlists(created_at DESC);

CREATE TRIGGER update_playlists_updated_at
    BEFORE UPDATE ON playlists
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

COMMENT ON TABLE playlists IS 'Playlist table supporting top-level and nested static or dynamic folders.';
COMMENT ON COLUMN playlists.name IS 'Playlist display name. It is not a routing key or unique path segment.';
COMMENT ON COLUMN playlists.parent_id IS 'Parent playlist ID';
COMMENT ON COLUMN playlists.position IS 'Floating-point order position in parent directory';
COMMENT ON COLUMN playlists.source_provider IS 'Media provider type name';
COMMENT ON COLUMN playlists.source_config IS 'Media provider configuration';
COMMENT ON COLUMN playlists.provider_instance_name IS 'Optional media provider instance name';
COMMENT ON CONSTRAINT playlists_dynamic_source_consistent ON playlists IS
    'Dynamic playlists have both source provider and config; static playlists have neither';
COMMENT ON COLUMN playlists.version IS 'Optimistic locking version, incremented on each update';

CREATE OR REPLACE FUNCTION check_playlist_cycle(
    playlist_id CHAR(12),
    new_parent_id CHAR(12)
) RETURNS BOOLEAN AS $$
DECLARE
    cycle_detected BOOLEAN;
BEGIN
    IF new_parent_id IS NULL THEN
        RETURN FALSE;
    END IF;

    SELECT EXISTS (
        WITH RECURSIVE ancestors AS (
            SELECT id, parent_id, 0 AS depth
            FROM playlists
            WHERE id = new_parent_id

            UNION ALL

            SELECT p.id, p.parent_id, a.depth + 1
            FROM playlists p
            JOIN ancestors a ON p.id = a.parent_id
            WHERE a.depth < 50
        )
        SELECT 1
        FROM ancestors
        WHERE id = playlist_id
    ) INTO cycle_detected;

    RETURN cycle_detected;
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION check_playlist_cycle(CHAR, CHAR) IS
    'Return TRUE when assigning parent_id would create a playlist cycle';

CREATE OR REPLACE FUNCTION prevent_playlist_cycle()
RETURNS TRIGGER AS $$
BEGIN
    IF (TG_OP = 'INSERT' AND NEW.parent_id IS NOT NULL) OR
       (TG_OP = 'UPDATE' AND NEW.parent_id IS DISTINCT FROM OLD.parent_id AND NEW.parent_id IS NOT NULL) THEN

        IF check_playlist_cycle(NEW.id, NEW.parent_id) THEN
            RAISE EXCEPTION 'Circular reference detected: setting parent_id=% for playlist % would create a cycle',
                NEW.parent_id, NEW.id
                USING ERRCODE = '23514',
                      HINT = 'Cannot set a descendant as parent';
        END IF;
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_prevent_playlist_cycle
    BEFORE INSERT OR UPDATE OF parent_id ON playlists
    FOR EACH ROW
    EXECUTE FUNCTION prevent_playlist_cycle();

COMMENT ON TRIGGER trigger_prevent_playlist_cycle ON playlists IS
    'Prevent circular references in playlist tree';

CREATE OR REPLACE FUNCTION validate_parent_same_room()
RETURNS TRIGGER AS $$
DECLARE
    parent_room_id CHAR(12);
BEGIN
    IF NEW.parent_id IS NULL THEN
        RETURN NEW;
    END IF;

    IF TG_OP = 'UPDATE' AND NEW.parent_id IS NOT DISTINCT FROM OLD.parent_id THEN
        RETURN NEW;
    END IF;

    SELECT room_id INTO parent_room_id
    FROM playlists
    WHERE id = NEW.parent_id;

    IF parent_room_id IS NULL THEN
        RAISE EXCEPTION 'Parent playlist % does not exist', NEW.parent_id
            USING ERRCODE = '23503';
    END IF;

    IF parent_room_id != NEW.room_id THEN
        RAISE EXCEPTION 'Cross-room parent_id violation: playlist % in room % cannot have parent % from room %',
            NEW.id, NEW.room_id, NEW.parent_id, parent_room_id
            USING ERRCODE = '23514',
                  HINT = 'Parent playlist must belong to the same room';
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION validate_parent_same_room() IS
    'Enforce room isolation for playlist parent references';

CREATE TRIGGER trigger_validate_parent_same_room
    BEFORE INSERT OR UPDATE OF parent_id, room_id ON playlists
    FOR EACH ROW
    EXECUTE FUNCTION validate_parent_same_room();

COMMENT ON TRIGGER trigger_validate_parent_same_room ON playlists IS
    'Prevent cross-room parent playlist references';
