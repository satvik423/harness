-- Phase 5 (ADR-045): pinned facts keep the session-wide id the openers show
-- (`p3`), so the table matches the latest `Compacted` record.
ALTER TABLE pinned_facts ADD COLUMN pin_id INTEGER;
ALTER TABLE pinned_facts ADD COLUMN superseded_by INTEGER;
CREATE UNIQUE INDEX pinned_facts_session_pin ON pinned_facts(session_id, pin_id);
