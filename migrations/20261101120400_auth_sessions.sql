-- Signed-in devices and clients (the auth spec). Each token is stored only as its SHA-256 hash,
-- which a request's token is looked up by (AUTH-007, DATA-22). A device has at most one session
-- per user (AUTH-008).
CREATE TABLE sessions (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    device_id text,
    access_token_hash bytea NOT NULL,
    refresh_token_hash bytea NOT NULL,
    previous_refresh_token_hash bytea,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    access_token_expires_at timestamptz NOT NULL,
    refresh_token_expires_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    CONSTRAINT sessions_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    CONSTRAINT sessions_access_token_hash_key UNIQUE (access_token_hash),
    CONSTRAINT sessions_refresh_token_hash_key UNIQUE (refresh_token_hash),
    CONSTRAINT sessions_previous_refresh_token_hash_key UNIQUE (previous_refresh_token_hash)
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id);

CREATE UNIQUE INDEX sessions_user_id_device_id_key ON sessions (user_id, device_id)
WHERE device_id IS NOT NULL;

CREATE TRIGGER sessions_set_row_version
BEFORE UPDATE ON sessions
FOR EACH ROW EXECUTE FUNCTION set_row_version();
