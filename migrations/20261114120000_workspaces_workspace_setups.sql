-- The workspaces that the subcommand setup-workspace created, each with the name its run was
-- given, which no later run may use (WORKSPACES-027). A row is inserted and read, never updated.
-- Deleting the workspace deletes its row, so its name can be given again.
CREATE TABLE workspace_setups (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    workspace_id uuid NOT NULL,
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT workspace_setups_workspace_id_key UNIQUE (workspace_id),
    CONSTRAINT workspace_setups_name_key UNIQUE (name),
    CONSTRAINT workspace_setups_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES workspaces (id) ON DELETE CASCADE
);

REVOKE UPDATE ON workspace_setups FROM app;

-- The name a run was given is the workspace's first name, text a person writes (DATA-23).
COMMENT ON COLUMN workspace_setups.name IS 'personal_data';
