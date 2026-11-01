-- The job queue and the periodic jobs' schedules (the platform spec's Jobs). A unit of work's
-- enqueue inserts a job, and only the dispatcher changes it afterwards, so neither table has
-- updated_at or version (DATA-02).
--
-- A job waits until its run_at, and runs while a replica holds its lease: claim_id names the
-- claim, and lease_expires_at ends it unless a heartbeat renews it. A success deletes the row. A
-- job dead-lettered at dead_at keeps its payload until the sweep deletes it (OPS-10).
--
-- user_id and workspace_id are the scope the job runs for (PLATFORM-012). They hold no foreign
-- keys (DATA-05): erasing a user, or deleting a workspace, deletes the jobs enqueued for them
-- (DATA-28).
CREATE TYPE job_state AS ENUM ('waiting', 'dead');

CREATE TABLE jobs (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    kind text NOT NULL,
    payload jsonb NOT NULL,
    user_id uuid,
    workspace_id uuid,
    traceparent text,
    state job_state NOT NULL DEFAULT 'waiting',
    attempts integer NOT NULL DEFAULT 0,
    run_at timestamptz NOT NULL DEFAULT now(),
    claim_id uuid,
    lease_expires_at timestamptz,
    failure text,
    dead_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT jobs_claim_id_check CHECK ((claim_id IS NULL) = (lease_expires_at IS NULL)),
    CONSTRAINT jobs_dead_at_check CHECK ((state = 'dead') = (dead_at IS NOT NULL))
);

-- The jobs a dispatcher may claim, the longest-waiting first.
CREATE INDEX jobs_run_at_idx ON jobs (run_at) WHERE state = 'waiting';
-- The dead letters, the oldest first, for the sweep.
CREATE INDEX jobs_dead_at_idx ON jobs (dead_at) WHERE state = 'dead';

-- A payload holds what its job needs, such as an email's recipient (DATA-28).
COMMENT ON COLUMN jobs.payload IS 'personal_data';

-- One row per periodic job, by its name: when it runs next, and the lease of the replica running
-- it (OPS-11).
CREATE TABLE job_schedules (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    name text NOT NULL,
    next_run_at timestamptz NOT NULL,
    claim_id uuid,
    lease_expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT job_schedules_name_key UNIQUE (name),
    CONSTRAINT job_schedules_claim_id_check CHECK ((claim_id IS NULL) = (lease_expires_at IS NULL))
);
