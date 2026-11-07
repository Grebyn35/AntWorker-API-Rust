-- The files of each workspace (the files spec): what the API knows of each, whose bytes are the
-- object its object_key names in the bucket. Rows are never updated, so the table has neither
-- updated_at nor version (DATA-02), and app may only insert, read and delete them.
CREATE TABLE workspace_files (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    workspace_id uuid NOT NULL,
    name text NOT NULL,
    media_type text NOT NULL,
    size_bytes bigint NOT NULL,
    object_key text NOT NULL,
    uploader_id uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT workspace_files_object_key_key UNIQUE (object_key),
    CONSTRAINT workspace_files_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES workspaces (id) ON DELETE CASCADE,
    -- Until account deletion clears a user's uploads (D31).
    CONSTRAINT workspace_files_uploader_id_fkey FOREIGN KEY (uploader_id) REFERENCES users (id) ON DELETE RESTRICT
);

-- A workspace's files, the latest uploaded first (FILES-007), which also indexes the foreign key
-- (DATA-05).
CREATE INDEX workspace_files_workspace_id_created_at_id_idx ON workspace_files (workspace_id, created_at DESC, id DESC);
CREATE INDEX workspace_files_uploader_id_idx ON workspace_files (uploader_id);

-- A name is text a user writes (DATA-23).
COMMENT ON COLUMN workspace_files.name IS 'personal_data';

REVOKE UPDATE ON workspace_files FROM app;

-- The objects in the bucket that no file's row names, each with when its bytes may go
-- (FILES-011). A row is inserted, read and deleted, never updated.
CREATE TABLE object_deletions (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    object_key text NOT NULL,
    deletable_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT object_deletions_object_key_key UNIQUE (object_key)
);

-- The objects whose time has passed, for the deletion (FILES-011).
CREATE INDEX object_deletions_deletable_at_idx ON object_deletions (deletable_at);

REVOKE UPDATE ON object_deletions FROM app;
