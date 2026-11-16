-- The names of the secrets agents and the team hold, and which agent is given which team
-- secret. A value is never stored here: it lives only in the secret store (SSM, or the files
-- of BUZZ_HIVE_SECRETS_DIR in local development), which the engine writes and the host
-- supervisor reads. There is no column that could hold one.
CREATE TABLE hive.team_secrets (
    community_id uuid        NOT NULL,
    var          text        NOT NULL,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    updated_by   text        NOT NULL,
    PRIMARY KEY (community_id, var)
);

-- An agent's own secret, set for that agent alone.
CREATE TABLE hive.agent_secrets (
    community_id uuid        NOT NULL,
    agent        text        NOT NULL,
    var          text        NOT NULL,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    updated_by   text        NOT NULL,
    PRIMARY KEY (community_id, agent, var),
    FOREIGN KEY (community_id, agent) REFERENCES hive.agent_tool_agents (community_id, agent)
        ON DELETE CASCADE
);

-- The team secrets an agent is given. An agent with no row here gets no team secret.
CREATE TABLE hive.agent_secret_grants (
    community_id uuid NOT NULL,
    agent        text NOT NULL,
    var          text NOT NULL,
    PRIMARY KEY (community_id, agent, var),
    FOREIGN KEY (community_id, agent) REFERENCES hive.agent_tool_agents (community_id, agent)
        ON DELETE CASCADE,
    FOREIGN KEY (community_id, var) REFERENCES hive.team_secrets (community_id, var)
        ON DELETE CASCADE
);
