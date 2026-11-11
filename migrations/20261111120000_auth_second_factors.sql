-- An account's authenticator app, one at most (AUTH-056). A check computes the app's codes from
-- its secret, so the secret is encrypted with AES-256-GCM under the version of ENCRYPTION_KEYS
-- that key_version names, bound to its table, its column and its row (SEC-02, AUTH-066).
-- last_used_step is the last 30-second step whose code the app accepted, so each step's code works
-- once (AUTH-057).
CREATE TABLE totp_factors (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    key_version smallint NOT NULL,
    secret bytea NOT NULL,
    last_used_step bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT totp_factors_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    -- One app per account, which also indexes the foreign key (DATA-05).
    CONSTRAINT totp_factors_user_id_key UNIQUE (user_id)
);

-- The apps whose secret another version of the key ring holds, which the hourly job encrypts
-- again, and which startup counts (AUTH-066).
CREATE INDEX totp_factors_key_version_idx ON totp_factors (key_version);

CREATE TRIGGER totp_factors_set_row_version
BEFORE UPDATE ON totp_factors
FOR EACH ROW EXECUTE FUNCTION set_row_version();

-- A setup of an authenticator app that its confirmation hasn't ended, one at most per account,
-- which lasts 15 minutes (AUTH-055), with its secret encrypted as an app's is. A row is never
-- edited: a new setup deletes the one before it, and a confirmation deletes its own, so app may
-- not update one (DATA-19).
CREATE TABLE totp_setups (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    key_version smallint NOT NULL,
    secret bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    CONSTRAINT totp_setups_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    -- One setup per account, which also indexes the foreign key (DATA-05).
    CONSTRAINT totp_setups_user_id_key UNIQUE (user_id)
);

-- The setups that expired, which the hourly job deletes (AUTH-067).
CREATE INDEX totp_setups_expires_at_idx ON totp_setups (expires_at);

REVOKE UPDATE ON totp_setups FROM app;

-- An account's recovery codes, ten at most, each of which works once in place of its app's code
-- (AUTH-062). A code is stored only as the SHA-256 of its user's ID and its characters, whose 120
-- bits need no slower hash, as a token's don't (SEC-03), and which a change of HMAC_KEY leaves. A row is never edited: a new set deletes the one
-- before it, and using a code deletes it, so app may not update one (DATA-19).
CREATE TABLE recovery_codes (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    code_hash bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT recovery_codes_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
);

-- An account's codes, which also indexes the foreign key (DATA-05).
CREATE INDEX recovery_codes_user_id_idx ON recovery_codes (user_id);

REVOKE UPDATE ON recovery_codes FROM app;

-- The challenge of a sign-in whose password matched, for an account with an app, which a code
-- completes within 5 minutes (AUTH-058, AUTH-059). Its token is stored only as its SHA-256 hash,
-- which a completion looks it up by (SEC-03, DATA-16). It holds what the session it starts needs:
-- its device's ID and name, and the hash of the device token that the sign-in brought, when that
-- was valid for the email (AUTH-019). A row is never edited: a completion, a reset, a change of
-- the password, a removal of the app and the hourly job delete it, so app may not update one
-- (DATA-19).
CREATE TABLE sign_in_challenges (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    token_hash bytea NOT NULL,
    device_id text,
    device_name text,
    device_token_hash bytea,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    CONSTRAINT sign_in_challenges_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    CONSTRAINT sign_in_challenges_token_hash_key UNIQUE (token_hash)
);

-- A user's challenges, which a reset deletes, and which also indexes the foreign key (DATA-05).
CREATE INDEX sign_in_challenges_user_id_idx ON sign_in_challenges (user_id);

-- The challenges that expired, which the hourly job deletes (AUTH-067).
CREATE INDEX sign_in_challenges_expires_at_idx ON sign_in_challenges (expires_at);

COMMENT ON COLUMN sign_in_challenges.device_name IS 'personal_data';

REVOKE UPDATE ON sign_in_challenges FROM app;
