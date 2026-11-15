CREATE TABLE hive.access_levels (
    community_id uuid        NOT NULL,
    pubkey       text        NOT NULL,
    level        smallint    NOT NULL CHECK (level BETWEEN 0 AND 3),
    kind         text        NOT NULL CHECK (kind IN ('person', 'agent')),
    updated_by   text        NOT NULL,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (community_id, pubkey)
);

CREATE TABLE hive.settings (
    community_id uuid  NOT NULL,
    scope        text  NOT NULL CHECK (scope IN ('engine', 'workflow', 'card')),
    scope_id     text  NOT NULL DEFAULT '',
    key          text  NOT NULL,
    value        jsonb NOT NULL,
    PRIMARY KEY (community_id, scope, scope_id, key)
);
