-- What a verification code proves: an account's email address (AUTH-030), or that its requester
-- may reset its password (AUTH-036).
CREATE TYPE verification_purpose AS ENUM ('email_verification', 'password_reset');

-- The codes that an account proves its email address or resets its password with, one at a time
-- for each purpose (AUTH-030, AUTH-036). A code is stored only as an HMAC under HMAC_KEY of its
-- user, its purpose and its characters, so no dump holds one (SEC-03). A row is never edited: a
-- new code deletes the one before it, and using a code deletes it, so app may not update one
-- (DATA-19).
CREATE TABLE verification_codes (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    purpose verification_purpose NOT NULL,
    code_hash bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    CONSTRAINT verification_codes_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    -- One code at a time for each purpose, which also indexes the foreign key (DATA-05).
    CONSTRAINT verification_codes_user_id_purpose_key UNIQUE (user_id, purpose)
);

REVOKE UPDATE ON verification_codes FROM app;
