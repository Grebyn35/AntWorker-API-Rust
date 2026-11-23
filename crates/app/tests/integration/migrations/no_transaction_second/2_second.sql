-- no-transaction
-- Marked to run outside a transaction, and no build of an index, so migrate refuses it, and
-- applies nothing, the migration before it included (PLATFORM-005).
CREATE TABLE second (id integer);
