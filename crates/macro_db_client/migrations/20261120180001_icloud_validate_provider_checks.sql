SET LOCAL lock_timeout = '1s';
SET LOCAL statement_timeout = '15min';

-- These scans use SHARE UPDATE EXCLUSIVE rather than the schema-changing lock
-- that 20261120180000_icloud_provider took to replace the checks.
ALTER TABLE calendar_accounts VALIDATE CONSTRAINT calendar_accounts_provider_check;
ALTER TABLE calendar_event_sources VALIDATE CONSTRAINT calendar_event_sources_source_kind_check;
ALTER TABLE calendar_events VALIDATE CONSTRAINT calendar_events_canonical_source_kind_check;
