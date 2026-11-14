CREATE TABLE hive.workflow_packages (
    community_id uuid        NOT NULL,
    name         text        NOT NULL,
    builtin      boolean     NOT NULL DEFAULT false,
    current_hash text        NOT NULL,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (community_id, name)
);

CREATE TABLE hive.workflow_versions (
    community_id uuid        NOT NULL,
    name         text        NOT NULL,
    hash         text        NOT NULL,
    body         jsonb       NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    created_by   text        NOT NULL,
    PRIMARY KEY (community_id, name, hash)
);

CREATE TABLE hive.workflow_options (
    community_id uuid  NOT NULL,
    name         text  NOT NULL,
    options      jsonb NOT NULL DEFAULT '{}',
    PRIMARY KEY (community_id, name)
);
