-- One piece of work the assistant agent does for a person: a chat turn, a workflow edit or a
-- skill adaptation. `root_event_id` is the thread the agent works in, and names the job's
-- conversation: a later chat turn is a new job on the same root. `result` holds what the agent
-- reported. `deadline` is when a job still running is ended as failed; empty means never.
CREATE TABLE hive.jobs (
    community_id  uuid        NOT NULL,
    id            uuid        NOT NULL,
    kind          text        NOT NULL,
    requester     text        NOT NULL,
    state         text        NOT NULL CHECK (state IN ('running', 'done', 'cancelled', 'failed')),
    channel_id    uuid        NOT NULL,
    root_event_id text,
    agent_pubkey  text        NOT NULL,
    result        jsonb,
    created_at    timestamptz NOT NULL DEFAULT now(),
    ended_at      timestamptz,
    deadline      timestamptz,
    PRIMARY KEY (community_id, id)
);
-- At most one job runs in a thread.
CREATE UNIQUE INDEX jobs_running_per_thread
    ON hive.jobs (community_id, root_event_id) WHERE state = 'running' AND root_event_id IS NOT NULL;
-- The sweep that ends overdue jobs reads only running ones.
CREATE INDEX jobs_deadline ON hive.jobs (community_id, deadline) WHERE state = 'running';
