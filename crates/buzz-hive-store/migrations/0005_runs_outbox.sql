-- A run is one pass of a card through its workflow. `visit` counts the card's visits to
-- `current_stage` across runs, so a stage's outbox keys never repeat between runs.
CREATE TABLE hive.runs (
    community_id  uuid        NOT NULL,
    id            uuid        NOT NULL,
    card_id       uuid        NOT NULL,
    workflow      text        NOT NULL,
    workflow_hash text        NOT NULL,
    state         text        NOT NULL
                  CHECK (state IN ('created','running','waiting','paused','ended')),
    current_stage text,
    visit         int         NOT NULL DEFAULT 0 CHECK (visit >= 0),
    requester     text,       -- pubkey of whoever started the run
    started_at    timestamptz NOT NULL DEFAULT now(),
    ended_at      timestamptz,
    PRIMARY KEY (community_id, id),
    FOREIGN KEY (community_id, card_id) REFERENCES hive.cards (community_id, id) ON DELETE CASCADE
);
CREATE INDEX runs_by_card ON hive.runs (community_id, card_id, started_at);
-- A card has at most one run that has not ended.
CREATE UNIQUE INDEX runs_one_active_per_card ON hive.runs (community_id, card_id)
    WHERE state <> 'ended';

-- Posts waiting for the relay, written in the transaction that causes them. `key` is the
-- idempotency key: queueing a key twice adds one row.
CREATE TABLE hive.outbox (
    id           bigserial   PRIMARY KEY,
    community_id uuid        NOT NULL,
    key          text        NOT NULL,
    card_id      uuid        NOT NULL,
    effect       jsonb       NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    done_at      timestamptz,
    attempts     int         NOT NULL DEFAULT 0,
    last_error   text,
    UNIQUE (community_id, key),
    FOREIGN KEY (community_id, card_id) REFERENCES hive.cards (community_id, id) ON DELETE CASCADE
);
CREATE INDEX outbox_pending ON hive.outbox (community_id, id) WHERE done_at IS NULL;

-- The relay thread a stage task opened.
CREATE TABLE hive.stage_threads (
    community_id  uuid NOT NULL,
    card_id       uuid NOT NULL,
    stage         text NOT NULL,
    visit         int  NOT NULL,
    root_event_id text NOT NULL,
    agent_pubkey  text NOT NULL,
    PRIMARY KEY (community_id, card_id, stage, visit),
    FOREIGN KEY (community_id, card_id) REFERENCES hive.cards (community_id, id) ON DELETE CASCADE
);
