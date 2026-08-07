CREATE TABLE IF NOT EXISTS file_object_groups (
    id TEXT PRIMARY KEY,
    storage_backend TEXT NOT NULL,
    original_object_key TEXT NOT NULL,
    media_kind TEXT NOT NULL,
    metadata JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (jsonb_typeof(metadata) = 'object'),
    FOREIGN KEY (storage_backend, original_object_key)
        REFERENCES file_objects(storage_backend, object_key) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_file_object_groups_original
    ON file_object_groups(storage_backend, original_object_key);

CREATE TABLE IF NOT EXISTS file_object_variants (
    storage_backend TEXT NOT NULL,
    object_key TEXT NOT NULL,
    original_storage_backend TEXT NOT NULL,
    original_object_key TEXT NOT NULL,
    group_id TEXT NOT NULL,
    variant_key TEXT NOT NULL,
    label TEXT NOT NULL,
    url TEXT,
    mime_type TEXT NOT NULL,
    size_bytes BIGINT NOT NULL,
    width INTEGER,
    height INTEGER,
    is_original BOOLEAN NOT NULL DEFAULT FALSE,
    lossy BOOLEAN NOT NULL DEFAULT FALSE,
    quality INTEGER,
    sort_order INTEGER NOT NULL DEFAULT 0,
    metadata JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (size_bytes > 0),
    CHECK (width IS NULL OR width > 0),
    CHECK (height IS NULL OR height > 0),
    CHECK (quality IS NULL OR (quality >= 1 AND quality <= 100)),
    CHECK (jsonb_typeof(metadata) = 'object'),
    PRIMARY KEY (storage_backend, object_key),
    UNIQUE (group_id, variant_key),
    FOREIGN KEY (storage_backend, object_key)
        REFERENCES file_objects(storage_backend, object_key) ON DELETE CASCADE,
    FOREIGN KEY (group_id)
        REFERENCES file_object_groups(id) ON DELETE CASCADE,
    FOREIGN KEY (original_storage_backend, original_object_key)
        REFERENCES file_objects(storage_backend, object_key) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_file_object_variants_original
    ON file_object_variants(original_storage_backend, original_object_key, sort_order);
CREATE INDEX IF NOT EXISTS idx_file_object_variants_group
    ON file_object_variants(group_id, sort_order);
