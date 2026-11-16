-- What the pull request sync keeps of each pull request beyond its state: whether a reviewer
-- approved it, and how its checks stand (`passing`, `failing` or `pending`, empty when none
-- were read). The card page shows the checks, and the merge button waits on them.
ALTER TABLE hive.pull_requests
    ADD COLUMN approved boolean NOT NULL DEFAULT false,
    ADD COLUMN checks   text    NOT NULL DEFAULT ''
        CHECK (checks IN ('', 'passing', 'failing', 'pending'));
