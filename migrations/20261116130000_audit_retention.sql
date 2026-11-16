-- Retention (the audit spec's "Retention"): the hourly job audit.expire_entries deletes each entry
-- recorded more than AUDIT_RETENTION_DAYS ago (AUDIT-011).

-- The entries of every log, the oldest first, which the job deletes in batches.
CREATE INDEX audit_entries_created_at_idx ON audit_entries (created_at);

-- The entries recorded more than 90 days before the transaction that reads them began, through
-- which retention deletes. Postgres deletes through the view only the rows it shows, with owner's
-- privileges on audit_entries, so app, which holds DELETE on the view and none on the table,
-- deletes through it no entry recorded in the last 90 days (AUDIT-005, D168). Each entry's client
-- goes with it, through its foreign key.
CREATE VIEW deletable_audit_entries AS
SELECT id, created_at FROM audit_entries
WHERE created_at < now() - interval '90 days';

-- The first migration's default privileges grant app all four; it reads and deletes alone
-- (DATA-19).
REVOKE INSERT, UPDATE ON deletable_audit_entries FROM app;
