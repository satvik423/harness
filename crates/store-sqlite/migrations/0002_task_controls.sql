-- Task control requests between processes (ADR-031): `harness task pause ID` in
-- one terminal, picked up by the process running the session at a safe point.
CREATE TABLE task_controls (
  id INTEGER PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  task_id TEXT NOT NULL,
  control TEXT NOT NULL,                   -- pause | resume | cancel
  created_at INTEGER NOT NULL
);
CREATE INDEX task_controls_session ON task_controls(session_id, id);
