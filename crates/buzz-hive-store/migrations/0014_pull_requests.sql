-- The pull request a card's `open_pull_request` stage opened.
--
-- One row per card: a second visit to the stage finds the open pull request for the same branch
-- and keeps it. `head` is the card branch and `base` the branch it merges into. `state` is
-- `open`, `closed` or `merged`. The worktrees table and the `worktree.prepared` event of the Go
-- engine have no counterpart: a card's work is on its branch in git, and each agent keeps its own
-- checkout.
CREATE TABLE hive.pull_requests (
    community_id uuid        NOT NULL,
    card_id      uuid        NOT NULL,
    repo         text        NOT NULL,
    number       bigint      NOT NULL CHECK (number > 0),
    url          text        NOT NULL DEFAULT '',
    head         text        NOT NULL,
    base         text        NOT NULL DEFAULT '',
    state        text        NOT NULL DEFAULT 'open' CHECK (state IN ('open','closed','merged')),
    draft        boolean     NOT NULL DEFAULT true,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (community_id, card_id),
    FOREIGN KEY (community_id, card_id) REFERENCES hive.cards (community_id, id) ON DELETE CASCADE
);
