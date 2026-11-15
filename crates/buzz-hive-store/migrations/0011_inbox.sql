-- Needs you: each place a run waits for a person to decide. An item is written in the
-- transaction that makes the run wait, and closed by the answer, or by the run moving on.
-- `visit` and `stage` say where the run was; `detail` holds what is kind-specific, such as
-- the artifacts an approval lists. `resolved_by` is empty for an item the engine closed itself.
CREATE TABLE hive.inbox (
    community_id uuid        NOT NULL,
    id           uuid        NOT NULL,
    card_id      uuid        NOT NULL,
    run_id       uuid        NOT NULL,
    stage        text        NOT NULL,
    visit        int         NOT NULL CHECK (visit >= 0),
    kind         text        NOT NULL
                 CHECK (kind IN ('approval','question','route','no_result','missing_output','failure')),
    title        text        NOT NULL,
    detail       jsonb       NOT NULL DEFAULT '{}'::jsonb,
    opened_at    timestamptz NOT NULL DEFAULT now(),
    resolved_at  timestamptz,
    resolved_by  text,
    answer       text,
    PRIMARY KEY (community_id, id),
    FOREIGN KEY (community_id, card_id) REFERENCES hive.cards (community_id, id) ON DELETE CASCADE,
    FOREIGN KEY (community_id, run_id) REFERENCES hive.runs (community_id, id) ON DELETE CASCADE
);
-- The list a person opens: open items, newest first.
CREATE INDEX inbox_open ON hive.inbox (community_id, opened_at DESC) WHERE resolved_at IS NULL;
CREATE INDEX inbox_by_card ON hive.inbox (community_id, card_id, opened_at DESC);
