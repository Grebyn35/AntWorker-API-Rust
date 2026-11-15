-- Every workspace of the deployment, the latest created first, which a superadmin lists
-- (WORKSPACES-029).
CREATE INDEX workspaces_created_at_id_idx ON workspaces (created_at DESC, id DESC);
