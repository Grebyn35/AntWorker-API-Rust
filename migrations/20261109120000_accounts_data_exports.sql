-- Where an export stands: its archive being built, built, or its job's last attempt failed
-- (ACCOUNTS-022).
CREATE TYPE data_export_state AS ENUM ('pending', 'ready', 'failed');

-- Each export a user requested (ACCOUNTS-020): only its job and its expiry change a row.
-- Deleting the user deletes their exports, with what their archives hold.
CREATE TABLE data_exports (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    state data_export_state NOT NULL DEFAULT 'pending',
    ready_at timestamptz,
    size_bytes bigint,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT data_exports_state_check CHECK ((state = 'ready') = (ready_at IS NOT NULL AND size_bytes IS NOT NULL)),
    CONSTRAINT data_exports_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE TRIGGER data_exports_set_row_version
BEFORE UPDATE ON data_exports
FOR EACH ROW EXECUTE FUNCTION set_row_version();

-- A user's exports, the latest first (ACCOUNTS-022), which also indexes the foreign key
-- (DATA-05).
CREATE INDEX data_exports_user_id_created_at_id_idx ON data_exports (user_id, created_at DESC, id DESC);
-- The exports whose time has passed, for their expiry (ACCOUNTS-027).
CREATE INDEX data_exports_expires_at_idx ON data_exports (expires_at);

-- Each JSON document of an export's archive, at its position among them (the accounts spec's
-- "Exporting"). Rows are never updated.
CREATE TABLE data_export_entries (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    export_id uuid NOT NULL,
    position integer NOT NULL,
    path text NOT NULL,
    size_bytes bigint NOT NULL,
    stored_bytes bigint NOT NULL,
    crc32 bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- Its leading column also serves as the index of the foreign key (DATA-05).
    CONSTRAINT data_export_entries_export_id_position_key UNIQUE (export_id, position),
    CONSTRAINT data_export_entries_export_id_fkey FOREIGN KEY (export_id) REFERENCES data_exports (id) ON DELETE CASCADE
);

REVOKE UPDATE ON data_export_entries FROM app;

-- The deflated bytes of each document, in pieces of at most 1 MiB, each at its offset among them
-- (PLATFORM-084). Rows are never updated.
CREATE TABLE data_export_chunks (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    export_id uuid NOT NULL,
    position integer NOT NULL,
    offset_bytes bigint NOT NULL,
    bytes bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- Its leading column also serves as the index of the foreign key (DATA-05).
    CONSTRAINT data_export_chunks_export_id_position_offset_bytes_key UNIQUE (export_id, position, offset_bytes),
    CONSTRAINT data_export_chunks_export_id_fkey FOREIGN KEY (export_id) REFERENCES data_exports (id) ON DELETE CASCADE
);

-- What a user keeps, deflated (DATA-23).
COMMENT ON COLUMN data_export_chunks.bytes IS 'personal_data';

REVOKE UPDATE ON data_export_chunks FROM app;
