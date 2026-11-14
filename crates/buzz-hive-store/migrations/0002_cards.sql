CREATE TABLE hive.cards (
    community_id uuid        NOT NULL,
    id           uuid        NOT NULL,
    title        text        NOT NULL,
    description  text        NOT NULL DEFAULT '',
    state        text        NOT NULL DEFAULT 'idle'
                 CHECK (state IN ('idle','running','waiting','failed','done','stopped')),
    workflow     text        NOT NULL DEFAULT '',
    archived_at  timestamptz,
    created_by   text        NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (community_id, id)
);
CREATE INDEX cards_by_state ON hive.cards (community_id, state) WHERE archived_at IS NULL;
