-- Published events, kept for replay (the platform spec's events). An event of the user scope
-- goes to user_id's streams, and one of the workspace scope to the streams of workspace_id's
-- members who hold its permission (SEC-17). payload holds IDs only (API-19). Only a unit of
-- work's publish_to_user and publish_to_workspace write a row, so the table has no updated_at or
-- version (DATA-02).
CREATE TYPE event_scope AS ENUM ('user', 'workspace');

CREATE TABLE events (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    scope event_scope NOT NULL,
    user_id uuid,
    workspace_id uuid,
    type text NOT NULL,
    permission text,
    payload jsonb NOT NULL,
    traceparent text,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT events_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    CONSTRAINT events_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES workspaces (id) ON DELETE CASCADE,
    CONSTRAINT events_scope_check CHECK (
        (scope = 'user' AND user_id IS NOT NULL AND workspace_id IS NULL AND permission IS NULL)
        OR (scope = 'workspace' AND workspace_id IS NOT NULL AND user_id IS NULL AND permission IS NOT NULL)
    )
);

CREATE INDEX events_user_id_idx ON events (user_id);
CREATE INDEX events_workspace_id_idx ON events (workspace_id);
