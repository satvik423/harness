-- Tech spec 3.12. Pragmas are set on every connection, not here.
CREATE TABLE sessions (
  id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, title TEXT,
  persona TEXT NOT NULL, cwd TEXT NOT NULL, status TEXT NOT NULL,        -- open | closed
  parent_id TEXT REFERENCES sessions(id), forked_at_seq INTEGER,
  current_segment INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE tasks (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
  parent_task TEXT REFERENCES tasks(id), persona TEXT NOT NULL,
  state TEXT NOT NULL, state_reason TEXT, budget_json TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE TABLE events (
  id INTEGER PRIMARY KEY,                 -- global rowid (used by FTS)
  session_id TEXT NOT NULL REFERENCES sessions(id),
  seq INTEGER NOT NULL,                   -- per-session, gapless, starts at 1
  task_id TEXT, segment INTEGER NOT NULL, ts INTEGER NOT NULL,
  kind TEXT NOT NULL,                     -- Event variant name
  payload TEXT NOT NULL,                  -- JSON; large bodies -> blob refs
  tokens INTEGER,                         -- tokens as sent (capped form)
  UNIQUE (session_id, seq)
);
CREATE INDEX events_task ON events(task_id, seq);
CREATE VIRTUAL TABLE events_fts USING fts5(body, session_id UNINDEXED, tokenize = 'porter unicode61');
CREATE TABLE segments (
  session_id TEXT NOT NULL, segment INTEGER NOT NULL,
  start_seq INTEGER NOT NULL, end_seq INTEGER, opener_seq INTEGER,
  tokens_before INTEGER, tokens_after INTEGER,
  PRIMARY KEY (session_id, segment)
);
CREATE TABLE anchors (
  session_id TEXT NOT NULL, anchor_id TEXT NOT NULL,
  segment INTEGER NOT NULL, from_seq INTEGER NOT NULL, to_seq INTEGER NOT NULL,
  topic TEXT NOT NULL, has_json TEXT NOT NULL, tokens INTEGER NOT NULL,
  PRIMARY KEY (session_id, anchor_id)
);
CREATE TABLE pinned_facts (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, text TEXT NOT NULL,
  source_seq INTEGER NOT NULL, kind TEXT NOT NULL,
  active INTEGER NOT NULL DEFAULT 1, created_at INTEGER NOT NULL
);
CREATE TABLE blobs (
  hash TEXT PRIMARY KEY, media_type TEXT NOT NULL, bytes INTEGER NOT NULL,
  original_name TEXT, provider_file_ids TEXT, created_at INTEGER NOT NULL
);
CREATE TABLE snapshots (
  session_id TEXT NOT NULL, seq INTEGER NOT NULL, path TEXT NOT NULL,
  blob_hash TEXT,                          -- NULL = file did not exist before
  PRIMARY KEY (session_id, seq, path)
);
CREATE TABLE usage (
  task_id TEXT NOT NULL, turn INTEGER NOT NULL, provider TEXT, model TEXT,
  input INTEGER, output INTEGER, cache_read INTEGER, cache_write INTEGER,
  cost_micros INTEGER, PRIMARY KEY (task_id, turn)
);
CREATE TABLE memories (
  id TEXT PRIMARY KEY, scope TEXT NOT NULL,
  path TEXT NOT NULL, title TEXT, body TEXT NOT NULL, hash TEXT NOT NULL,
  pinned INTEGER NOT NULL DEFAULT 0, uses INTEGER NOT NULL DEFAULT 0,
  last_used_at INTEGER, updated_at INTEGER NOT NULL
);
CREATE VIRTUAL TABLE memories_fts USING fts5(title, body, tokenize = 'porter unicode61');
