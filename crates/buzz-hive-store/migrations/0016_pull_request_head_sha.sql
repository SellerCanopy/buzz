-- The head commit a pull request's `checks` were read for. A merge the workflow makes is sent
-- with it, so a push after the checks were read is refused by the host instead of merged
-- unchecked. Empty when no checks were read.
ALTER TABLE hive.pull_requests
    ADD COLUMN head_sha text NOT NULL DEFAULT '';
