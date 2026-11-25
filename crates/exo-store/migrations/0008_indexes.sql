CREATE INDEX IF NOT EXISTS "index_drafts_by_thread_id" ON "drafts" ("thread_id");
-- #[toasty::breakpoint]
CREATE INDEX IF NOT EXISTS "index_calendar_changes_by_token" ON "calendar_changes" ("token");
-- #[toasty::breakpoint]
CREATE INDEX IF NOT EXISTS "index_calendar_changes_by_queue" ON "calendar_changes" ("queue");
-- #[toasty::breakpoint]
CREATE INDEX IF NOT EXISTS "index_events_by_end" ON "events" ("end");
