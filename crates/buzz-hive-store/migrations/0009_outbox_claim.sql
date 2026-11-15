-- A drainer claims a post for a lease and sends it outside any transaction. A claim older
-- than its lease no longer counts, so a post whose drainer died is offered again.
ALTER TABLE hive.outbox ADD COLUMN claimed_at timestamptz;
