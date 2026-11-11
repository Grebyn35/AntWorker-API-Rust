-- What a verification code proves: an account's email address (AUTH-030), that its requester
-- may reset its password (AUTH-036), or that the account may change its address to the one the
-- code was sent to (AUTH-050).
CREATE TYPE verification_purpose AS ENUM ('email_verification', 'password_reset', 'email_change');

-- The codes that an account proves its email address, resets its password or changes its address
-- with: one code at a time that proves its address (AUTH-030), one at a time that changes it
-- (AUTH-050), and one reset code for each request for a reset, which lasts its 10 minutes or until
-- a reset uses one, and which the next one's issue deletes once it has expired (AUTH-036). A code
-- is stored only as an HMAC under HMAC_KEY of its user, its purpose, the address it was sent to
-- and its characters, so no dump holds one, and none proves anything for another address
-- (SEC-03). A row is never edited: a new code that proves the address, or changes it, deletes the
-- one before it, and using a code deletes it, so app may not update one (DATA-19).
CREATE TABLE verification_codes (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    purpose verification_purpose NOT NULL,
    code_hash bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    CONSTRAINT verification_codes_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
);

-- An account's codes of one purpose, which also indexes the foreign key (DATA-05).
CREATE INDEX verification_codes_user_id_purpose_idx ON verification_codes (user_id, purpose);

-- One code at a time that proves an account's address, and one that changes it (AUTH-030,
-- AUTH-050).
CREATE UNIQUE INDEX verification_codes_user_id_purpose_key ON verification_codes (user_id, purpose)
WHERE purpose <> 'password_reset';

REVOKE UPDATE ON verification_codes FROM app;
