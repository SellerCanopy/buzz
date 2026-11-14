CREATE TABLE hive.events (
    seq          bigserial   PRIMARY KEY,
    community_id uuid        NOT NULL,
    kind         text        NOT NULL,
    card_id      uuid,
    run_id       uuid,
    actor_pubkey text        NOT NULL,
    actor_kind   text        NOT NULL CHECK (actor_kind IN ('person','agent')),
    payload      jsonb       NOT NULL DEFAULT '{}',
    at           timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX events_by_community_seq ON hive.events (community_id, seq);
CREATE INDEX events_by_card ON hive.events (community_id, card_id, seq) WHERE card_id IS NOT NULL;
