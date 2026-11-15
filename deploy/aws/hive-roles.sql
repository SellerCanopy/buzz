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
-- Read access is by column, and only the columns RELAY_COLUMNS in
-- crates/buzz-hive-store/src/relay_read.rs lists. A table-wide grant would also expose
-- every other column the relay keeps, so the table grant is revoked first (a stack set
-- up earlier had it). Add a column here when RELAY_COLUMNS gains one: the service checks
-- the list at startup and the roles test compares it with these grants.
REVOKE SELECT ON public.communities, public.channels, public.channel_members, public.relay_members, public.users, public.events FROM buzz_hive;
GRANT SELECT (id, host) ON public.communities TO buzz_hive;
GRANT SELECT (community_id, id, archived_at, deleted_at) ON public.channels TO buzz_hive;
GRANT SELECT (community_id, channel_id, pubkey, removed_at) ON public.channel_members TO buzz_hive;
GRANT SELECT (community_id, pubkey, role) ON public.relay_members TO buzz_hive;
GRANT SELECT (community_id, pubkey, agent_owner_pubkey, display_name, deactivated_at) ON public.users TO buzz_hive;
GRANT SELECT (community_id, id, pubkey, created_at, kind, tags, channel_id, deleted_at) ON public.events TO buzz_hive;
