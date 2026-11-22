-- The publisher of an event: the user whose request published it, as the Principal that opened
-- its unit of work proved them, and null for an event of a unit of work that no Principal opened,
-- such as a job's. A stream's frame of the event carries the event's trace ID only when the
-- stream is its publisher's (REALTIME-002, SEC-19). Deleting the publisher's account sets it to
-- null (DATA-05, DATA-23). The key is added without a read of the table's rows, which a later
-- migration makes, so that neither holds up an event that a request publishes (DATA-10).
ALTER TABLE events
    ADD COLUMN publisher_id uuid,
    ADD CONSTRAINT events_publisher_id_fkey FOREIGN KEY (publisher_id) REFERENCES users (id) ON DELETE SET NULL NOT VALID;
