-- no-transaction
-- Every workspace of the deployment, the latest created first, which a superadmin lists
-- (WORKSPACES-029). It is built outside a transaction, so that its build holds up no write of
-- a workspace (DATA-10, PLATFORM-005).
CREATE INDEX CONCURRENTLY IF NOT EXISTS workspaces_created_at_id_idx ON workspaces (created_at DESC, id DESC);
