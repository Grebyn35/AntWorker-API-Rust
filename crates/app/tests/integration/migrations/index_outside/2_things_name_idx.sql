-- no-transaction
-- Builds an index outside a transaction, as the one statement of its migration (PLATFORM-005).
CREATE INDEX CONCURRENTLY IF NOT EXISTS things_name_idx ON things (name);
