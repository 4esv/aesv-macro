-- Connects the stack's iCloud account to one user's calendar (W-2066).
--
-- psql -v macro_id='macro|you@example.com' -v owner=<macro_user.id> \
--      -v apple_id=<ICLOUD_APPLE_ID> -f deploy/aesv/connect-icloud.sql
--
-- It writes what a Google grant writes: an ICLOUD email link, the link's
-- calendar grant, an icloud calendar account and a backfill job with its
-- outbox row. The worker reads ICLOUD_APPLE_ID and ICLOUD_APP_PASSWORD from
-- the stack's env, and the five-minute due-sync poll keeps it fresh.
--
-- The grant row reuses the Google calendar scopes because every calendar job
-- gate reads email_link_google_scopes. The link starts with mail sync off.
-- A second run fails on email_uq_links_user_email_provider and changes nothing.
--
-- NOTE(shortcut): one iCloud account per stack, connected by hand; replace with
-- a link route that takes an app password once the app connects iCloud itself.
BEGIN;
WITH link AS (
    INSERT INTO email_links (id, macro_id, fusionauth_user_id, email_address, provider, is_sync_active)
    VALUES (gen_random_uuid(), :'macro_id', :'owner', lower(:'apple_id'), 'ICLOUD', false)
    RETURNING id
), scopes AS (
    INSERT INTO email_link_google_scopes (link_id, granted_scopes, grant_version)
    SELECT id,
           ARRAY['https://www.googleapis.com/auth/calendar.events',
                 'https://www.googleapis.com/auth/calendar.calendarlist.readonly'],
           1
    FROM link
), account AS (
    INSERT INTO calendar_accounts (id, owner_id, email_link_id, provider, provider_account_id)
    SELECT gen_random_uuid(), :'macro_id', id, 'icloud', lower(:'apple_id') FROM link
    RETURNING id, email_link_id
), job AS (
    INSERT INTO calendar_backfill_jobs (id, email_link_id, account_id, kind, grant_version)
    SELECT gen_random_uuid(), email_link_id, id, 'google_calendar', 1 FROM account
    RETURNING id
)
INSERT INTO calendar_sync_outbox (id, backfill_job_id)
SELECT gen_random_uuid(), id FROM job;
COMMIT;
