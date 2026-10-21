-- Fails after creating its table, so the table must roll back with it.
CREATE TABLE broken (id integer PRIMARY KEY);
SELECT 1 / 0;
