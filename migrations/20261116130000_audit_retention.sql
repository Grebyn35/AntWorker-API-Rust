-- Retention (the audit spec's "Retention"): the hourly job audit.expire_entries deletes each entry
-- recorded more than AUDIT_RETENTION_DAYS ago (AUDIT-011), through an index that the next
-- migration builds.

-- The entries recorded more than 90 days before the transaction that reads them began, through
-- which retention deletes. Postgres deletes through the view only the rows it shows, with owner's
-- privileges on audit_entries, so app, which holds DELETE on the view and none on the table,
-- deletes through it no entry recorded in the last 90 days (AUDIT-005, D168). Each entry's client
-- goes with it, through its foreign key. The 90 days are 2,160 hours: Postgres subtracts an
-- interval of days in the session's time zone, which any role may set, and a zone that app sets
-- could shorten 90 days by two weeks.
CREATE VIEW deletable_audit_entries AS
SELECT id, created_at FROM audit_entries
WHERE created_at < now() - interval '2160 hours';

-- The first migration's default privileges grant app all four; it reads and deletes alone
-- (DATA-19).
REVOKE INSERT, UPDATE ON deletable_audit_entries FROM app;
