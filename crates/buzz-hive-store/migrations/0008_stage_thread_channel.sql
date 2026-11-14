-- A stage thread is read from the card's Live tab, which opens it in the channel the task
-- was posted in. That channel is the update channel when the task was queued, and the
-- setting can change afterwards, so the thread keeps its own. `seq` keeps the order the
-- threads were opened in, which the primary key does not.
ALTER TABLE hive.stage_threads
    ADD COLUMN channel_id uuid,
    ADD COLUMN seq        bigint GENERATED ALWAYS AS IDENTITY;

-- The outbox row that opened a thread is kept, and holds the channel it was posted in.
UPDATE hive.stage_threads t
   SET channel_id = (o.effect ->> 'channel')::uuid
  FROM hive.outbox o
 WHERE o.community_id = t.community_id
   AND o.card_id = t.card_id
   AND o.effect ->> 'kind' = 'stage_task'
   AND o.effect ->> 'stage' = t.stage
   AND (o.effect ->> 'visit')::int = t.visit;

ALTER TABLE hive.stage_threads ALTER COLUMN channel_id SET NOT NULL;
