-- Signed-in devices and clients (the auth spec). Each token is stored only as its SHA-256 hash,
-- which a request's token is looked up by (AUTH-007, DATA-16). A device has at most one session
-- per user (AUTH-008), and device_name is the name its client gave the device, if any (AUTH-042).
-- authenticated_at is when the user last proved who they are in the session, their password and
-- their second factor when they have one: when it started, re-authenticated (AUTH-021), or changed
-- the password (AUTH-040).
CREATE TABLE sessions (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    device_id text,
    device_name text,
    access_token_hash bytea NOT NULL,
    refresh_token_hash bytea NOT NULL,
    previous_refresh_token_hash bytea,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    access_token_expires_at timestamptz NOT NULL,
    refresh_token_expires_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    authenticated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT sessions_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    CONSTRAINT sessions_access_token_hash_key UNIQUE (access_token_hash),
    CONSTRAINT sessions_refresh_token_hash_key UNIQUE (refresh_token_hash),
    CONSTRAINT sessions_previous_refresh_token_hash_key UNIQUE (previous_refresh_token_hash)
);

-- A user's sessions, the latest first, which their list (AUTH-043) and an export (ACCOUNTS-024) read
-- a page at a time, and which also indexes the foreign key (DATA-05).
CREATE INDEX sessions_user_id_created_at_id_idx ON sessions (user_id, created_at DESC, id DESC);

-- The sessions whose refresh token expired, which the hourly job deletes (AUTH-047).
CREATE INDEX sessions_refresh_token_expires_at_idx ON sessions (refresh_token_expires_at);

CREATE UNIQUE INDEX sessions_user_id_device_id_key ON sessions (user_id, device_id)
WHERE device_id IS NOT NULL;

COMMENT ON COLUMN sessions.device_name IS 'personal_data';

CREATE TRIGGER sessions_set_row_version
BEFORE UPDATE ON sessions
FOR EACH ROW EXECUTE FUNCTION set_row_version();
