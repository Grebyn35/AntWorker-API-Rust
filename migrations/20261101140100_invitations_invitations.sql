-- Offers to join a workspace at a role, each to an email address (the invitations spec). An
-- invitation never makes its invitee the owner (ACCESS-001). Refreshing an invitation replaces
-- its role, inviter and expiry in place (INVITATIONS-003), so the table has updated_at and
-- version (DATA-02).
CREATE TABLE invitations (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    workspace_id uuid NOT NULL,
    email text NOT NULL,
    role workspace_role NOT NULL,
    inviter_id uuid NOT NULL,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT invitations_workspace_id_email_key UNIQUE (workspace_id, email),
    CONSTRAINT invitations_role_check CHECK (role <> 'owner'),
    CONSTRAINT invitations_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES workspaces (id) ON DELETE CASCADE,
    -- Until account deletion removes a user's invitations (D16).
    CONSTRAINT invitations_inviter_id_fkey FOREIGN KEY (inviter_id) REFERENCES users (id) ON DELETE RESTRICT
);

-- An invitee's invitations, the latest first (INVITATIONS-004).
CREATE INDEX invitations_email_created_at_id_idx ON invitations (email, created_at DESC, id DESC);
CREATE INDEX invitations_inviter_id_idx ON invitations (inviter_id);
-- The expired invitations, for the sweep (INVITATIONS-007).
CREATE INDEX invitations_expires_at_idx ON invitations (expires_at);

-- The invitee's address (DATA-28).
COMMENT ON COLUMN invitations.email IS 'personal_data';

CREATE TRIGGER invitations_set_row_version
BEFORE UPDATE ON invitations
FOR EACH ROW EXECUTE FUNCTION set_row_version();
