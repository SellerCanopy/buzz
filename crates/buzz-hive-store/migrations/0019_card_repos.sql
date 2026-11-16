-- The repos a card works in besides its issue's. A card with no issue has only these. `seq`
-- is the order they were added in, so the first repo of a card is well defined: its issue's,
-- else the lowest `seq`. The rest are linked context.
CREATE TABLE hive.card_repos (
    community_id uuid   NOT NULL,
    card_id      uuid   NOT NULL,
    repo         text   NOT NULL,
    seq          bigint GENERATED ALWAYS AS IDENTITY,
    PRIMARY KEY (community_id, card_id, repo),
    FOREIGN KEY (community_id, card_id) REFERENCES hive.cards (community_id, id) ON DELETE CASCADE
);
CREATE INDEX card_repos_by_card ON hive.card_repos (community_id, card_id, seq);
