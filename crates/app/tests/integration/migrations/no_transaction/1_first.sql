-- no-transaction
-- Runs outside a transaction, so migrate refuses it (PLATFORM-028).
CREATE TABLE first (id integer);
