-- Who a user is, and how they appear to others (the accounts spec). email holds the normalized
-- address (ACCOUNTS-002), and no two users share one (ACCOUNTS-004).
CREATE TABLE users (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    email text NOT NULL,
    name text NOT NULL,
    email_verified boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT users_email_key UNIQUE (email)
);

CREATE TRIGGER users_set_row_version
BEFORE UPDATE ON users
FOR EACH ROW EXECUTE FUNCTION set_row_version();

COMMENT ON COLUMN users.email IS 'personal_data';
COMMENT ON COLUMN users.name IS 'personal_data';
