-- A migration that a transaction holds, which migrate applies nothing of while the next one
-- stands beside it (PLATFORM-005).
CREATE TABLE first (id integer PRIMARY KEY);
