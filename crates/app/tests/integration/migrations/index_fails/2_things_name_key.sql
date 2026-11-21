-- no-transaction
-- Builds a unique index outside a transaction, which fails while a name stands twice
-- (PLATFORM-005).
CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS things_name_key ON things (name);
