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
    trace_id text,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- A user's own log belongs to their account.
    CONSTRAINT audit_entries_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    -- Deleting the user anonymizes the entries of other logs that name them (AUDIT-006).
    CONSTRAINT audit_entries_actor_user_id_fkey FOREIGN KEY (actor_user_id) REFERENCES users (id) ON DELETE SET NULL,
    CONSTRAINT audit_entries_target_user_id_fkey FOREIGN KEY (target_user_id) REFERENCES users (id) ON DELETE SET NULL,
    CONSTRAINT audit_entries_scope_check CHECK (
        (scope = 'user' AND user_id IS NOT NULL AND workspace_id IS NULL)
        OR (scope = 'workspace' AND workspace_id IS NOT NULL AND user_id IS NULL)
    )
);

-- Each log's entries, the latest first, which an export reads a page at a time (ACCOUNTS-024),
-- and which also index the foreign keys (DATA-05).
CREATE INDEX audit_entries_user_id_created_at_id_idx ON audit_entries (user_id, created_at DESC, id DESC);
CREATE INDEX audit_entries_workspace_id_created_at_id_idx ON audit_entries (workspace_id, created_at DESC, id DESC);
CREATE INDEX audit_entries_actor_user_id_idx ON audit_entries (actor_user_id);
CREATE INDEX audit_entries_target_user_id_idx ON audit_entries (target_user_id);

-- The client of an entry recorded in a request. user_id is its actor, or, for an entry without
-- one, the user whose log holds it, so deleting that user deletes it, while the entry stays
-- (AUDIT-006).
CREATE TABLE audit_entry_clients (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    entry_id uuid NOT NULL,
    user_id uuid NOT NULL,
    client_ip inet,
    user_agent text,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT audit_entry_clients_entry_id_key UNIQUE (entry_id),
    CONSTRAINT audit_entry_clients_entry_id_fkey FOREIGN KEY (entry_id) REFERENCES audit_entries (id) ON DELETE CASCADE,
    CONSTRAINT audit_entry_clients_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE INDEX audit_entry_clients_user_id_idx ON audit_entry_clients (user_id);

COMMENT ON COLUMN audit_entry_clients.client_ip IS 'personal_data';
COMMENT ON COLUMN audit_entry_clients.user_agent IS 'personal_data';

-- No request changes or deletes an entry (AUDIT-005).
REVOKE UPDATE, DELETE ON audit_entries, audit_entry_clients FROM app;
