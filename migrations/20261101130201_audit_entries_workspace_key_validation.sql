-- Reads the entries against the key that the migration before this one added, under a lock
-- that lets a request record an entry meanwhile (DATA-10).
ALTER TABLE audit_entries VALIDATE CONSTRAINT audit_entries_workspace_id_fkey;
