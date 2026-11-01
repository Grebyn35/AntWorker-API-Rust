-- A workspace's log goes with the workspace (the audit spec).
ALTER TABLE audit_entries
ADD CONSTRAINT audit_entries_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES workspaces (id) ON DELETE CASCADE;
