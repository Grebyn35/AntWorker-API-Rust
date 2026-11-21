-- no-transaction
-- The entries of every log, the oldest first, which retention's job deletes in batches
-- (AUDIT-011). It is built outside a transaction, so that its build holds up no entry that a
-- request records (DATA-10, PLATFORM-005).
CREATE INDEX CONCURRENTLY IF NOT EXISTS audit_entries_created_at_idx ON audit_entries (created_at);
