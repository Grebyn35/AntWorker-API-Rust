-- Who belongs to each workspace, at which role (the access spec). created_at is when the member
-- joined. A member's role can change, so the table isn't a pure join table, and has an id
-- (DATA-02).
CREATE TYPE workspace_role AS ENUM ('owner', 'admin', 'member');

CREATE TABLE workspace_memberships (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    role workspace_role NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT workspace_memberships_workspace_id_user_id_key UNIQUE (workspace_id, user_id),
    CONSTRAINT workspace_memberships_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES workspaces (id) ON DELETE CASCADE,
    -- Until account deletion removes a user's memberships (D16).
    CONSTRAINT workspace_memberships_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE RESTRICT
);

-- Each workspace has exactly one owner (ACCESS-001).
CREATE UNIQUE INDEX workspace_memberships_workspace_id_key ON workspace_memberships (workspace_id) WHERE role = 'owner';
-- A user's workspaces, the latest joined first (WORKSPACES-003).
CREATE INDEX workspace_memberships_user_id_created_at_id_idx ON workspace_memberships (user_id, created_at DESC, id DESC);
-- A workspace's members, the latest joined first (WORKSPACES-004).
CREATE INDEX workspace_memberships_workspace_id_created_at_id_idx ON workspace_memberships (workspace_id, created_at DESC, id DESC);

CREATE TRIGGER workspace_memberships_set_row_version
BEFORE UPDATE ON workspace_memberships
FOR EACH ROW EXECUTE FUNCTION set_row_version();
