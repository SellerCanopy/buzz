-- What a run waits on: a stage's timeout, or the end of a retry's wait. A row is written in
-- the transaction that changes the run, so a restarted service finds every timer. `fire_at`
-- is the database's own time, so every instance and every restart agree on when it is due.
CREATE TABLE hive.timers (
    community_id uuid        NOT NULL,
    run_id       uuid        NOT NULL,
    stage        text        NOT NULL,
    kind         text        NOT NULL CHECK (kind IN ('timeout','retry')),
    fire_at      timestamptz NOT NULL,
    PRIMARY KEY (community_id, run_id, stage, kind),
    FOREIGN KEY (community_id, run_id) REFERENCES hive.runs (community_id, id) ON DELETE CASCADE
);
CREATE INDEX timers_by_fire_at ON hive.timers (community_id, fire_at);
