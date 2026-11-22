-- A workspace's log goes with the workspace (the audit spec). The key is added without a read
-- of the table's rows, which the next migration makes, so that neither holds up an entry that
-- a request records (DATA-10).
ALTER TABLE audit_entries
ADD CONSTRAINT audit_entries_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES workspaces (id) ON DELETE CASCADE NOT VALID;
