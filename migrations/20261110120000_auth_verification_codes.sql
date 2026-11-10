-- What a verification code proves: an account's email address (AUTH-030), or that its requester
-- may reset its password (AUTH-036).
CREATE TYPE verification_purpose AS ENUM ('email_verification', 'password_reset');

-- The codes that an account proves its email address or resets its password with: one code at a
-- time that proves its address (AUTH-030), and one reset code for each request for a reset, which
-- lasts its 10 minutes or until a reset uses one (AUTH-036). A code is stored only as an HMAC under
-- HMAC_KEY of its user, its purpose and its characters, so no dump holds one (SEC-03). A row is
-- never edited: a new code that proves the address deletes the one before it, and using a code
-- deletes it, so app may not update one (DATA-19).
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

-- One code at a time that proves an account's address (AUTH-030).
CREATE UNIQUE INDEX verification_codes_user_id_key ON verification_codes (user_id)
WHERE purpose = 'email_verification';

REVOKE UPDATE ON verification_codes FROM app;
