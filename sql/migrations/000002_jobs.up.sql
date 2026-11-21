-- Background jobs (rb-jobs). "Scheduled" and "retryable" are available rows whose scheduled_at is
-- in the future; the fetch query's scheduled_at <= now() does the scheduler's job.
CREATE TABLE rb_job (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind          text        NOT NULL,
    queue         text        NOT NULL DEFAULT 'default',
    args          jsonb       NOT NULL,
    state         text        NOT NULL DEFAULT 'available'
                  CHECK (state IN ('available', 'running', 'completed', 'discarded', 'cancelled')),
    priority      smallint    NOT NULL DEFAULT 1,
    attempt       int         NOT NULL DEFAULT 0,
    max_attempts  int         NOT NULL DEFAULT 25,
    scheduled_at  timestamptz NOT NULL DEFAULT now(),
    attempted_at  timestamptz,
    attempted_by  text,
    finalized_at  timestamptz,
    errors        jsonb       NOT NULL DEFAULT '[]',
    unique_key    bytea,
    -- Keep unique_key after finishing (period uniqueness); args-only keys are released.
    unique_keep   boolean     NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX rb_job_fetch ON rb_job (queue, priority, scheduled_at, id) WHERE state = 'available';
CREATE INDEX rb_job_running ON rb_job (attempted_at) WHERE state = 'running';
CREATE INDEX rb_job_finalized ON rb_job (finalized_at) WHERE finalized_at IS NOT NULL;
CREATE UNIQUE INDEX rb_job_unique ON rb_job (unique_key) WHERE unique_key IS NOT NULL;

CREATE TABLE rb_periodic (
    name        text PRIMARY KEY,
    next_run_at timestamptz NOT NULL
);
