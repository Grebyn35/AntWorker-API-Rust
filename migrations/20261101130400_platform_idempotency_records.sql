-- The operations committed under an idempotency key (API-12, PLATFORM-018). The operation's own
-- unit of work inserts its row, and completes it with the result at its commit, so a row exists
-- exactly when its operation committed. A record older than 24 hours counts as absent, and the
-- next claim of its key replaces it. Only the claim and its commit change a row, so the table has
-- no updated_at or version (DATA-02).
CREATE TABLE idempotency_records (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id uuid NOT NULL,
    key text NOT NULL,
    fingerprint bytea NOT NULL,
    result jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT idempotency_records_user_id_fkey FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    CONSTRAINT idempotency_records_user_id_key_key UNIQUE (user_id, key)
);
