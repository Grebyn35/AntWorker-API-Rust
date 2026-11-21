-- no-transaction
-- Marked to run outside a transaction, and no build of an index, so migrate refuses it
-- (PLATFORM-005).
CREATE TABLE first (id integer);
