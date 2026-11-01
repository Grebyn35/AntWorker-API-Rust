-- Who did what (the audit spec). An entry of the user scope is in user_id's log, and one of the
-- workspace scope in workspace_id's, whose foreign key a later migration adds, once the
-- workspaces table exists.
CREATE TYPE audit_scope AS ENUM ('user', 'workspace');

CREATE TABLE audit_entries (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    scope audit_scope NOT NULL,
    user_id uuid,
    workspace_id uuid,
    actor_user_id uuid,
    target_user_id uuid,
    action text NOT NULL,
    resource_type text NOT NULL,
    resource_id uuid,
    details jsonb NOT NULL DEFAULT '{}',
    client_ip inet,
    user_agent text,
    trace_id text,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- A user's own log belongs to their account.
    CONSTRAINT audit_entries_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    -- Until the full module's erasure anonymizes them (D31).
    CONSTRAINT audit_entries_actor_user_id_fkey FOREIGN KEY (actor_user_id) REFERENCES users (id) ON DELETE RESTRICT,
    CONSTRAINT audit_entries_target_user_id_fkey FOREIGN KEY (target_user_id) REFERENCES users (id) ON DELETE RESTRICT,
    CONSTRAINT audit_entries_scope_check CHECK (
        (scope = 'user' AND user_id IS NOT NULL AND workspace_id IS NULL)
        OR (scope = 'workspace' AND workspace_id IS NOT NULL AND user_id IS NULL)
    )
);

CREATE INDEX audit_entries_user_id_idx ON audit_entries (user_id);
CREATE INDEX audit_entries_workspace_id_idx ON audit_entries (workspace_id);
CREATE INDEX audit_entries_actor_user_id_idx ON audit_entries (actor_user_id);
CREATE INDEX audit_entries_target_user_id_idx ON audit_entries (target_user_id);

COMMENT ON COLUMN audit_entries.client_ip IS 'personal_data';
COMMENT ON COLUMN audit_entries.user_agent IS 'personal_data';

-- No request changes or deletes an entry (AUDIT-005).
REVOKE UPDATE, DELETE ON audit_entries FROM app;
