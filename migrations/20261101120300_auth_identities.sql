-- The ways a user signs in (the auth spec). A password identity holds the password's hash in the
-- string format of the Password Hashing Competition (AUTH-014). A user has at most one identity of
-- each method.
CREATE TYPE identity_method AS ENUM ('password');

CREATE TABLE identities (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    method identity_method NOT NULL,
    password_hash text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT identities_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    -- Its leading column also serves as the index of the foreign key (DATA-05).
    CONSTRAINT identities_user_id_method_key UNIQUE (user_id, method),
    CONSTRAINT identities_password_hash_check CHECK (method <> 'password' OR password_hash IS NOT NULL)
);

CREATE TRIGGER identities_set_row_version
BEFORE UPDATE ON identities
FOR EACH ROW EXECUTE FUNCTION set_row_version();
