-- Every table whose rows are edited has updated_at and version, which this one trigger function
-- maintains (DATA-02): an update sets updated_at to the database's clock (DATA-03) and adds one
-- to version, which a conditional update checks (DATA-24). It is the only function the schema
-- defines (DATA-07).
CREATE FUNCTION set_row_version() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.updated_at := now();
    NEW.version := OLD.version + 1;
    RETURN NEW;
END;
$$;
