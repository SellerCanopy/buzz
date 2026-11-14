-- Creates the buzz_hive role, its schema, and its read access to the relay tables.
-- Run once as the database owner by the hive-db-init service in compose.hive.yml:
--   psql -v hive_password=... -f hive-roles.sql
-- It is safe to run again. The role cannot grant itself access to relay tables it does
-- not own, so a buzz-hive migration cannot do this.
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'buzz_hive') THEN
    CREATE ROLE buzz_hive LOGIN;
  END IF;
END $$;
ALTER ROLE buzz_hive PASSWORD :'hive_password';
CREATE SCHEMA IF NOT EXISTS hive AUTHORIZATION buzz_hive;
ALTER ROLE buzz_hive SET search_path = hive;
GRANT USAGE ON SCHEMA public TO buzz_hive;
GRANT SELECT ON public.communities, public.channels, public.channel_members, public.relay_members, public.users TO buzz_hive;
