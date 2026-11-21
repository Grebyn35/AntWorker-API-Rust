-- no-transaction
-- The events that a user published, which deleting their account clears of them: the index of
-- the foreign key (DATA-05). It is built outside a transaction, so that its build holds up no
-- event that a request publishes (DATA-10, PLATFORM-005).
CREATE INDEX CONCURRENTLY IF NOT EXISTS events_publisher_id_idx ON events (publisher_id);
