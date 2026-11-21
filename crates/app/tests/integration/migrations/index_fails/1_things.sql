-- A table with one name twice, so the unique index of the next migration fails to build.
CREATE TABLE things (id integer, name text);
INSERT INTO things VALUES (1, 'a'), (2, 'a');
