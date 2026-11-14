-- A run is one pass through the workflow, so it holds every stage it entered. `steps` is that
-- list, oldest first, as the engine writes it (see `RunStep`): the loop limit, the Steps tab
-- and a stage's visit count all read it. `parameters` are the run's own settings by name.
ALTER TABLE hive.runs
    ADD COLUMN steps      jsonb NOT NULL DEFAULT '[]',
    ADD COLUMN parameters jsonb NOT NULL DEFAULT '{}';
