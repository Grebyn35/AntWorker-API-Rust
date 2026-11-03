-- Published events, kept for replay (the platform spec's events). An event of the user scope
-- goes to user_id's streams, and one of the workspace scope to the streams of workspace_id's
-- members who hold its permission (SEC-17). payload holds IDs only (API-19). A unit of work's
-- publish_to_user and publish_to_workspace insert a row, and its commit gives the row its
-- position, from events_position_seq, sets created_at to the time it does, and keeps in xact its
-- own transaction and in snapshot the transactions that had committed by then: positions follow
-- the order in which units of work commit (PLATFORM-071), and an event reaches a member only if
-- the transaction that added the membership is its own or among those (SEC-17), so a row has none
-- of them until its unit of work commits. Only that mechanism writes a row, so the table has no
-- updated_at or version (DATA-02).
CREATE TYPE event_scope AS ENUM ('user', 'workspace');

CREATE TABLE events (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    scope event_scope NOT NULL,
    user_id uuid,
    workspace_id uuid,
    name text NOT NULL,
    permission text,
    payload jsonb NOT NULL,
    traceparent text,
    position bigint,
    xact xid8,
    snapshot pg_snapshot,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT events_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    CONSTRAINT events_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES workspaces (id) ON DELETE CASCADE,
    CONSTRAINT events_position_key UNIQUE (position),
    CONSTRAINT events_scope_check CHECK (
        (scope = 'user' AND user_id IS NOT NULL AND workspace_id IS NULL AND permission IS NULL)
        OR (scope = 'workspace' AND workspace_id IS NOT NULL AND user_id IS NULL AND permission IS NOT NULL)
    )
);

CREATE SEQUENCE events_position_seq AS bigint OWNED BY events.position;

-- The commit of a unit of work that published takes the next positions as app.
GRANT USAGE ON SEQUENCE events_position_seq TO app;

-- A stream's replay reads its user's events, and its workspaces', by position.
CREATE INDEX events_user_id_position_idx ON events (user_id, position);
CREATE INDEX events_workspace_id_position_idx ON events (workspace_id, position);
