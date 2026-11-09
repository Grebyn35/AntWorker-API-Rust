-- The device tokens that a registration and each successful sign-in issue, with which a later
-- sign-in of the same email counts only in its token's own limit (AUTH-006, AUTH-019). Each token
-- is stored only as its SHA-256 hash, which a sign-in's token is looked up by, and its email only
-- as an HMAC under HMAC_KEY, so a token is valid for one email, and no dump holds either (SEC-03).
-- A row is never edited: a sign-in deletes the token it used, and inserts the next, so app may
-- not update one (DATA-19).
CREATE TABLE device_tokens (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    email_hash bytea NOT NULL,
    token_hash bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    CONSTRAINT device_tokens_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    CONSTRAINT device_tokens_token_hash_key UNIQUE (token_hash)
);

-- A user's device tokens, the latest first, which an export reads a page at a time
-- (ACCOUNTS-024), and which also indexes the foreign key (DATA-05).
CREATE INDEX device_tokens_user_id_created_at_id_idx ON device_tokens (user_id, created_at DESC, id DESC);

REVOKE UPDATE ON device_tokens FROM app;
