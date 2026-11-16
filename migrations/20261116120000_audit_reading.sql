-- What reading the logs takes (the audit spec's "Reading a workspace's log" and "Reading one's
-- own log").

-- The permission that reading an entry of a workspace's log takes beside audit.read, which an
-- entry records from its action, or null for an entry that whoever reads the log reads, such as
-- a refusal, and for every entry of a user's own log (the audit spec's "Reading a workspace's
-- log"). A replica of the previous release writes no permission, and its entries stay readable
-- with audit.read alone (DATA-10).
ALTER TABLE audit_entries ADD COLUMN permission text;

ALTER TABLE audit_entries
DROP CONSTRAINT audit_entries_scope_check,
ADD CONSTRAINT audit_entries_scope_check CHECK (
    (scope = 'user' AND user_id IS NOT NULL AND workspace_id IS NULL AND permission IS NULL)
    OR (scope = 'workspace' AND workspace_id IS NOT NULL AND user_id IS NULL)
);

-- Every client that an entry records is a request's, which has an IP address, and a replica of
-- the previous release writes one with each client too (DATA-10).
ALTER TABLE audit_entry_clients ALTER COLUMN client_ip SET NOT NULL;

-- The entries whose target is a user, the latest first, which their own read reads
-- (AUDIT-008), and which also index the foreign key (DATA-05).
DROP INDEX audit_entries_target_user_id_idx;
CREATE INDEX audit_entries_target_user_id_created_at_id_idx ON audit_entries (target_user_id, created_at DESC, id DESC);
