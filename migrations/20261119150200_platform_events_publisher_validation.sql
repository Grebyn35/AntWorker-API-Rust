-- Reads the events against the key of their publisher, which an earlier migration added, under
-- a lock that lets a request publish an event meanwhile (DATA-10).
ALTER TABLE events VALIDATE CONSTRAINT events_publisher_id_fkey;
