-- Each user's consent to the telemetry of their clients (the client_telemetry spec, D122): one
-- row for each user who chose, through PUT /v1/users/me/telemetry-consent, whether their clients
-- may send it. A user without a row never chose, and consents to nothing. The row a user's first
-- choice inserts holds that choice no one made, consented false at version 0, which its update
-- then changes, so the version moves with each choice, as the trigger moves it (DATA-02).
-- Deleting the account deletes its row too.
CREATE TABLE telemetry_consents (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    consented boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 0,
    CONSTRAINT telemetry_consents_user_id_key UNIQUE (user_id),
    CONSTRAINT telemetry_consents_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE TRIGGER telemetry_consents_set_row_version
BEFORE UPDATE ON telemetry_consents
FOR EACH ROW EXECUTE FUNCTION set_row_version();
