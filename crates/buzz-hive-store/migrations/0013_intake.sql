-- Intake: the rules that bring issues of one repo onto the board, and the issues read.
--
-- A rule reads one repo every `interval_s` seconds. `filter` holds the label, whether the issue
-- must be unassigned and the logins whose issues are taken (and whose comments reach an agent).
-- An empty `workflow` fills Backlog.
--
-- `hive.issues` holds the last read of each issue and what Beehive made of it. `card_id` has no
-- foreign key on purpose: a card a person removes leaves the id behind, which is how intake
-- knows to keep that issue off the board. `left_reason` is empty while the issue is in its
-- rule's search, then `closed` or `unmatched`. `last_comment_at` is how far comments are read.
CREATE TABLE hive.intake_rules (
    community_id uuid        NOT NULL,
    id           uuid        NOT NULL,
    name         text        NOT NULL DEFAULT '',
    repo         text        NOT NULL,
    filter       jsonb       NOT NULL DEFAULT '{}'::jsonb,
    workflow     text        NOT NULL DEFAULT '',
    enabled      boolean     NOT NULL DEFAULT true,
    interval_s   int         NOT NULL DEFAULT 900 CHECK (interval_s >= 60),
    created_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (community_id, id)
);

CREATE TABLE hive.issues (
    community_id    uuid        NOT NULL,
    repo            text        NOT NULL,
    number          bigint      NOT NULL CHECK (number > 0),
    card_id         uuid,
    rule_id         uuid,
    title           text        NOT NULL,
    body            text        NOT NULL DEFAULT '',
    labels          text[]      NOT NULL DEFAULT '{}',
    assignees       text[]      NOT NULL DEFAULT '{}',
    author          text        NOT NULL DEFAULT '',
    state           text        NOT NULL CHECK (state IN ('open','closed')),
    url             text        NOT NULL DEFAULT '',
    etag            text,
    left_reason     text        NOT NULL DEFAULT '' CHECK (left_reason IN ('','closed','unmatched')),
    last_seen       timestamptz NOT NULL DEFAULT now(),
    last_comment_at timestamptz,
    PRIMARY KEY (community_id, repo, number)
);
CREATE INDEX issues_by_card ON hive.issues (community_id, card_id) WHERE card_id IS NOT NULL;
CREATE INDEX issues_by_rule ON hive.issues (community_id, rule_id) WHERE rule_id IS NOT NULL;
