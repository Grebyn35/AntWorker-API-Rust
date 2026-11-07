-- The files of each workspace (the files spec). A file's name is edited (FILES-005), so
-- the table has updated_at and version (DATA-02).
CREATE TABLE files (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    workspace_id uuid NOT NULL,
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT files_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES workspaces (id) ON DELETE CASCADE
);

-- A workspace's files, the latest created first (FILES-003), which also indexes the foreign
-- key (DATA-05).
CREATE INDEX files_workspace_id_created_at_id_idx ON files (workspace_id, created_at DESC, id DESC);

-- A name is text a user writes (DATA-23).
COMMENT ON COLUMN files.name IS 'personal_data';

CREATE TRIGGER files_set_row_version
BEFORE UPDATE ON files
FOR EACH ROW EXECUTE FUNCTION set_row_version();
