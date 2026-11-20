-- iCloud mail and calendar (W-2066): an email link may be ICLOUD, and its
-- calendar account, sources and events may be icloud.
SET LOCAL lock_timeout = '1s';
SET LOCAL statement_timeout = '5s';

ALTER TYPE email_user_provider_enum ADD VALUE IF NOT EXISTS 'ICLOUD';

ALTER TABLE calendar_accounts DROP CONSTRAINT calendar_accounts_provider_check;
ALTER TABLE calendar_accounts ADD CONSTRAINT calendar_accounts_provider_check CHECK(provider IN ('google','outlook','icloud')) NOT VALID;
ALTER TABLE calendar_event_sources DROP CONSTRAINT calendar_event_sources_source_kind_check;
ALTER TABLE calendar_event_sources ADD CONSTRAINT calendar_event_sources_source_kind_check CHECK(source_kind IN ('google','outlook','icloud')) NOT VALID;
ALTER TABLE calendar_events DROP CONSTRAINT calendar_events_canonical_source_kind_check;
ALTER TABLE calendar_events ADD CONSTRAINT calendar_events_canonical_source_kind_check CHECK(canonical_source_kind IN ('google','outlook','icloud')) NOT VALID;
-- Validate the iCloud checks in their own migration.
