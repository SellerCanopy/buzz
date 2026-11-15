-- A requester claim names a chat event, and the run it belongs to is found through the
-- thread that event sits in.
CREATE INDEX stage_threads_by_root ON hive.stage_threads (community_id, root_event_id);
