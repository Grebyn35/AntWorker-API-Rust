-- The deployment's superadmins (the superadmins spec): one row for each user whom the operator made
-- one, through the subcommand superadmin, which alone inserts a row, and which deletes one (D62).
-- A row is never updated. Deleting the account deletes its row too.
CREATE TABLE superadmins (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT superadmins_user_id_key UNIQUE (user_id),
    CONSTRAINT superadmins_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
);

REVOKE UPDATE ON superadmins FROM app;
