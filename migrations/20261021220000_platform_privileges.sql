-- The app role reads and writes every table the owner role creates in the schema, and can
-- change nothing else (DATA-19). A table that needs less revokes the rest in its own
-- migration.
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO app;

-- No role executes a function the owner role creates unless it is granted that function
-- (DATA-19).
ALTER DEFAULT PRIVILEGES REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;

-- serve reads the applied migrations at startup, to refuse a schema from another release
-- (PLATFORM-030). The migrations table exists before this migration runs, so the default
-- privileges above don't reach it.
GRANT SELECT ON _sqlx_migrations TO app;
