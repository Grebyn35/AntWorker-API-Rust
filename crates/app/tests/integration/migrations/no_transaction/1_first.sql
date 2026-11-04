-- no-transaction
-- Runs outside a transaction, so migrate refuses it (PLATFORM-005).
CREATE TABLE first (id integer);
