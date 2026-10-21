-- Mirrors the first real migration: serve reads the migrations table at startup.
GRANT SELECT ON _sqlx_migrations TO app;

CREATE TABLE first (id integer PRIMARY KEY, note text);
