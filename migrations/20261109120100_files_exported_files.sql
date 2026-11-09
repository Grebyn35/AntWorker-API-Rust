-- Each file whose bytes an export's archive holds, naming its object, which stays in the bucket
-- while the export lasts (FILES-026). Rows are never updated. Deleting the export, or the user,
-- deletes them.
CREATE TABLE exported_files (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    export_id uuid NOT NULL,
    user_id uuid NOT NULL,
    position integer NOT NULL,
    path text NOT NULL,
    object_key text NOT NULL,
    size_bytes bigint NOT NULL,
    crc32 bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- Its leading column also serves as the index of the foreign key (DATA-05).
    CONSTRAINT exported_files_export_id_position_key UNIQUE (export_id, position),
    CONSTRAINT exported_files_export_id_fkey FOREIGN KEY (export_id) REFERENCES data_exports (id) ON DELETE CASCADE,
    CONSTRAINT exported_files_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE INDEX exported_files_user_id_idx ON exported_files (user_id);
-- The objects that an export holds, which the deletion of unreferenced objects looks up
-- (FILES-011).
CREATE INDEX exported_files_object_key_idx ON exported_files (object_key);

-- A path holds the file's name, which is text a user writes (DATA-23).
COMMENT ON COLUMN exported_files.path IS 'personal_data';

REVOKE UPDATE ON exported_files FROM app;
