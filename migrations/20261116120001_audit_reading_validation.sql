-- Reads the rows against the check and the `NOT NULL` that the migration before this one
-- added, each under a lock that lets a request record an entry meanwhile (DATA-10).
ALTER TABLE audit_entries VALIDATE CONSTRAINT audit_entries_scope_check;

ALTER TABLE audit_entry_clients VALIDATE CONSTRAINT audit_entry_clients_client_ip_not_null;
