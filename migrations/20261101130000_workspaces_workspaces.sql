-- The workspaces that hold a product's data (the workspaces spec). name holds the normalized
-- name (WORKSPACES-002).
CREATE TABLE workspaces (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1
);

CREATE TRIGGER workspaces_set_row_version
BEFORE UPDATE ON workspaces
FOR EACH ROW EXECUTE FUNCTION set_row_version();

-- A workspace's name is text a user writes, such as a person's name (DATA-28).
COMMENT ON COLUMN workspaces.name IS 'personal_data';
