-- A user's accounts at OAuth providers, through which they sign in (AUTH-073): the provider's ID,
-- such as google, and the subject, the provider's ID for the user, which it never gives another
-- user. A provider's account signs in to one user, and a user has one account at each provider. A
-- row is never edited: linking inserts one, and unlinking deletes it, so app may not update one
-- (DATA-19).
CREATE TABLE provider_identities (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    provider text NOT NULL,
    subject text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT provider_identities_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    CONSTRAINT provider_identities_provider_subject_key UNIQUE (provider, subject),
    -- Its leading column also serves as the index of the foreign key (DATA-05).
    CONSTRAINT provider_identities_user_id_provider_key UNIQUE (user_id, provider)
);

REVOKE UPDATE ON provider_identities FROM app;

-- The provider's ID for the user identifies them at the provider (D31).
COMMENT ON COLUMN provider_identities.subject IS 'personal_data';

-- The provider whose sign-in started a session, or null for a password (AUTH-016, AUTH-073), and
-- the provider of a challenge's sign-in, which the session it starts holds (AUTH-058).
ALTER TABLE sessions ADD COLUMN provider text;
ALTER TABLE sign_in_challenges ADD COLUMN provider text;
