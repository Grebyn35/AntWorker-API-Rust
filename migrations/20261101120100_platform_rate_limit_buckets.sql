-- The rate limits' counts (PLATFORM-048 to PLATFORM-051). A bucket counts the requests of its
-- current window, which starts with the first request after the last window ended. Its key is an
-- HMAC of its kind and of what it counts, so no row holds an IP address, an email or a user ID.
-- Only the rate limiter changes a row, so the table has no updated_at or version (DATA-02).
CREATE TABLE rate_limit_buckets (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    key_hash bytea NOT NULL,
    count integer NOT NULL,
    window_started_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT rate_limit_buckets_key_hash_key UNIQUE (key_hash)
);
