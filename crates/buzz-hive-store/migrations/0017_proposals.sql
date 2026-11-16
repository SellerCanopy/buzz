-- What an agent offered that a person has not decided yet. `kind` says what `body` holds, so
-- one table serves a workflow edit an agent could not save and, later, the assistant's offers.
-- `requester` is the pubkey the agent acted for, who may decide it. `resolved_at` is empty
-- while the proposal is open.
CREATE TABLE hive.proposals (
    community_id uuid        NOT NULL,
    id           bigint      GENERATED ALWAYS AS IDENTITY,
    kind         text        NOT NULL,
    body         jsonb       NOT NULL,
    requester    text        NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    resolved_at  timestamptz,
    PRIMARY KEY (community_id, id)
);
CREATE INDEX proposals_open ON hive.proposals (community_id, kind, id) WHERE resolved_at IS NULL;
