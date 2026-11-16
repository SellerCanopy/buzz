-- The tool servers each agent starts, and the agents known to the engine. An agent is known
-- once its folder under deploy/aws/agents was read at a start; that read also seeds its
-- servers, once. A server whose row a person removed does not come back at the next start,
-- because the agent row is already there.
CREATE TABLE hive.agent_tool_agents (
    community_id uuid NOT NULL,
    agent        text NOT NULL,
    PRIMARY KEY (community_id, agent)
);

-- `spec` holds a command and its args, or a url, and only `${NAME}` references to secrets.
-- It never holds a secret value.
CREATE TABLE hive.agent_tool_servers (
    community_id uuid NOT NULL,
    agent        text NOT NULL,
    name         text NOT NULL,
    transport    text NOT NULL CHECK (transport IN ('stdio', 'http')),
    spec         jsonb NOT NULL,
    PRIMARY KEY (community_id, agent, name),
    FOREIGN KEY (community_id, agent) REFERENCES hive.agent_tool_agents (community_id, agent)
        ON DELETE CASCADE
);
