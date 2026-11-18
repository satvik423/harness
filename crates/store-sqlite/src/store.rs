//! [`SqliteStore`]: the storage ports on `.harness/state.db` and `.harness/blobs/`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use harness_ports::{
    BlobMeta, BlobStore, EventLog, LoggedEvent, SearchHit, SessionRecord, SessionStatus,
    SessionStore, Snapshot, SnapshotStore, StoreError, TaskControl, TaskRecord, Undone,
};
use harness_protocol::{Envelope, Event, TaskState};
use harness_types::{BlobRef, Seq, SessionId, TaskId};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::lock::{LockError, SessionLock};
use crate::schema::{configure, migrate};
use crate::text::{fts_query, searchable, title_of};
use crate::writer::Writer;
use crate::{backend, blobs};

/// Name of the database file in `.harness/`.
pub const STATE_DB: &str = "state.db";

/// The `.gitignore` written into `.harness/` on first use (P3-T9).
pub const HARNESS_GITIGNORE: &str = "\
# Written by harness. Local state: never commit it.
state.db*
blobs/
exports/
locks/
";

/// SQLite-backed storage for one project.
///
/// Implements [`EventLog`], [`BlobStore`], [`SessionStore`] and
/// [`SnapshotStore`]. All writes go through one writer thread that commits in
/// batches and answers only after the commit; reads use a separate connection
/// on the blocking thread pool. Several processes may open the same project.
///
/// # Examples
/// ```no_run
/// let store = harness_store_sqlite::SqliteStore::open(std::path::Path::new("."))?;
/// println!("state in {}", store.harness_dir().display());
/// # Ok::<(), harness_ports::StoreError>(())
/// ```
pub struct SqliteStore {
    harness: PathBuf,
    blobs: PathBuf,
    writer: Writer,
    reader: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteStore")
            .field("harness", &self.harness)
            .finish_non_exhaustive()
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn state_name(state: TaskState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn parse_state(name: &str) -> Result<TaskState, StoreError> {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .map_err(|_| StoreError::Corrupt(format!("unknown task state {name:?}")))
}

fn parse_id<T: std::str::FromStr>(text: &str, what: &str) -> Result<T, StoreError> {
    text.parse()
        .map_err(|_| StoreError::Corrupt(format!("bad {what} id {text:?}")))
}

fn control_name(control: TaskControl) -> &'static str {
    match control {
        TaskControl::Pause => "pause",
        TaskControl::Resume => "resume",
        TaskControl::Cancel => "cancel",
    }
}

fn parse_control(name: &str) -> Result<TaskControl, StoreError> {
    match name {
        "pause" => Ok(TaskControl::Pause),
        "resume" => Ok(TaskControl::Resume),
        "cancel" => Ok(TaskControl::Cancel),
        other => Err(StoreError::Corrupt(format!(
            "unknown task control {other:?}"
        ))),
    }
}

impl SqliteStore {
    /// Opens (creating if needed) the store in `<project>/.harness/`: the
    /// database, the blob folder and the `.gitignore`. Blocking; call it before
    /// the async runtime starts or on a blocking thread.
    ///
    /// # Errors
    /// A backend error if the files cannot be created or a migration fails;
    /// [`StoreError::Corrupt`] if the database is newer than this build.
    pub fn open(project: &Path) -> Result<Self, StoreError> {
        let harness = project.join(".harness");
        let io = |e: std::io::Error| StoreError::Backend(format!("{}: {e}", harness.display()));
        std::fs::create_dir_all(&harness).map_err(io)?;
        let gitignore = harness.join(".gitignore");
        if !gitignore.exists() {
            std::fs::write(&gitignore, HARNESS_GITIGNORE).map_err(io)?;
        }
        let blobs = harness.join("blobs");
        std::fs::create_dir_all(&blobs).map_err(io)?;
        let db = harness.join(STATE_DB);

        let mut write_conn = Connection::open(&db).map_err(backend)?;
        configure(&write_conn)?;
        migrate(&mut write_conn)?;
        let read_conn = Connection::open_with_flags(
            &db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(backend)?;
        read_conn
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(backend)?;
        Ok(Self {
            writer: Writer::start(write_conn)?,
            reader: Arc::new(Mutex::new(read_conn)),
            harness,
            blobs,
        })
    }

    /// The project's `.harness/` folder.
    #[must_use]
    pub fn harness_dir(&self) -> &Path {
        &self.harness
    }

    /// Takes the session's lock, so no other process runs it at the same time.
    ///
    /// # Errors
    /// [`LockError::Held`] if another process holds it.
    pub fn lock_session(&self, session: SessionId) -> Result<SessionLock, LockError> {
        SessionLock::acquire(&self.harness.join("locks"), session)
    }

    /// `true` if another process holds the session's lock right now.
    #[must_use]
    pub fn is_session_locked(&self, session: SessionId) -> bool {
        matches!(self.lock_session(session), Err(LockError::Held(_)))
    }

    /// Runs a read on the blocking pool with the read connection.
    async fn read<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let reader = Arc::clone(&self.reader);
        tokio::task::spawn_blocking(move || {
            let conn = reader.lock().unwrap_or_else(PoisonError::into_inner);
            f(&conn)
        })
        .await
        .map_err(|e| StoreError::Backend(format!("read task failed: {e}")))?
    }
}

/// Inserts one event and updates the views that depend on it.
fn append_event(conn: &Connection, event: &LoggedEvent) -> Result<(), StoreError> {
    let envelope = &event.envelope;
    let kind = envelope.event.kind();
    if !envelope.event.is_persisted() {
        return Err(StoreError::NotPersistable(kind));
    }
    let session = envelope.session.to_string();
    let (persona, title): (String, Option<String>) = conn
        .query_row(
            "SELECT persona, title FROM sessions WHERE id = ?1",
            [&session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(backend)?
        .ok_or_else(|| StoreError::NotFound(format!("session {session}")))?;
    let last: Option<i64> = conn
        .query_row(
            "SELECT MAX(seq) FROM events WHERE session_id = ?1",
            [&session],
            |r| r.get(0),
        )
        .map_err(backend)?;
    let expected = last.map_or(1, |l| l + 1);
    let seq = to_i64(envelope.seq.0);
    if seq != expected {
        return Err(StoreError::Conflict(format!(
            "session {session}: expected sequence number {expected}, got {seq}"
        )));
    }
    let payload = serde_json::to_string(&envelope.event)
        .map_err(|e| StoreError::Backend(format!("cannot encode the event: {e}")))?;
    conn.execute(
        "INSERT INTO events (session_id, seq, task_id, segment, ts, kind, payload, tokens)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
        params![
            session,
            seq,
            event.task.map(|t| t.to_string()),
            event.segment,
            envelope.ts,
            kind,
            payload
        ],
    )
    .map_err(backend)?;
    let body = searchable(&envelope.event);
    if !body.is_empty() {
        conn.execute(
            "INSERT INTO events_fts (rowid, body, session_id) VALUES (last_insert_rowid(), ?1, ?2)",
            params![body, session],
        )
        .map_err(backend)?;
    }
    match &envelope.event {
        Event::TaskState {
            task, to, reason, ..
        } => {
            conn.execute(
                "INSERT INTO tasks (id, session_id, parent_task, persona, state, state_reason, budget_json, created_at, updated_at)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, ?6, ?6)
                 ON CONFLICT(id) DO UPDATE SET state = excluded.state,
                   state_reason = excluded.state_reason, updated_at = excluded.updated_at",
                params![task.to_string(), session, persona, state_name(*to), reason, envelope.ts],
            )
            .map_err(backend)?;
        }
        Event::UserInput { message, .. } if title.is_none() => {
            conn.execute(
                "UPDATE sessions SET title = ?1 WHERE id = ?2",
                params![title_of(message), session],
            )
            .map_err(backend)?;
        }
        Event::SegmentOpened { segment, .. } => {
            conn.execute(
                "INSERT OR REPLACE INTO segments (session_id, segment, start_seq, opener_seq) VALUES (?1, ?2, ?3, ?3)",
                params![session, segment, seq],
            )
            .map_err(backend)?;
            conn.execute(
                "UPDATE sessions SET current_segment = ?1 WHERE id = ?2",
                params![segment, session],
            )
            .map_err(backend)?;
        }
        Event::Usage {
            task,
            usage,
            cost_micros,
        } => {
            conn.execute(
                "INSERT INTO usage (task_id, turn, input, output, cache_read, cache_write, cost_micros)
                 VALUES (?1, (SELECT COUNT(*) + 1 FROM usage WHERE task_id = ?1), ?2, ?3, ?4, ?5, ?6)",
                params![
                    task.to_string(),
                    to_i64(usage.input),
                    to_i64(usage.output),
                    to_i64(usage.cache_read),
                    to_i64(usage.cache_write),
                    to_i64(*cost_micros)
                ],
            )
            .map_err(backend)?;
        }
        _ => {}
    }
    Ok(())
}

fn row_to_event(
    session: SessionId,
    seq: i64,
    ts: i64,
    task: Option<String>,
    segment: u32,
    payload: &str,
) -> Result<LoggedEvent, StoreError> {
    let event: Event = serde_json::from_str(payload)
        .map_err(|e| StoreError::Corrupt(format!("event #{seq}: {e}")))?;
    Ok(LoggedEvent {
        envelope: Envelope {
            seq: Seq(u64::try_from(seq).unwrap_or(0)),
            ts,
            session,
            event,
        },
        task: task.map(|t| parse_id(&t, "task")).transpose()?,
        segment,
    })
}

#[async_trait]
impl EventLog for SqliteStore {
    async fn append(&self, event: LoggedEvent) -> Result<(), StoreError> {
        self.writer
            .run(move |conn| append_event(conn, &event))
            .await
    }

    async fn last_seq(&self, session: SessionId) -> Result<Option<Seq>, StoreError> {
        self.read(move |conn| {
            let last: Option<i64> = conn
                .query_row(
                    "SELECT MAX(seq) FROM events WHERE session_id = ?1",
                    [session.to_string()],
                    |r| r.get(0),
                )
                .map_err(backend)?;
            Ok(last.map(|l| Seq(u64::try_from(l).unwrap_or(0))))
        })
        .await
    }

    async fn range(
        &self,
        session: SessionId,
        from: Seq,
        to: Seq,
    ) -> Result<Vec<LoggedEvent>, StoreError> {
        self.read(move |conn| {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT seq, ts, task_id, segment, payload FROM events
                     WHERE session_id = ?1 AND seq BETWEEN ?2 AND ?3 ORDER BY seq",
                )
                .map_err(backend)?;
            let rows = stmt
                .query_map(
                    params![session.to_string(), to_i64(from.0), to_i64(to.0)],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, u32>(3)?,
                            r.get::<_, String>(4)?,
                        ))
                    },
                )
                .map_err(backend)?;
            rows.map(|row| {
                let (seq, ts, task, segment, payload) = row.map_err(backend)?;
                row_to_event(session, seq, ts, task, segment, &payload)
            })
            .collect()
        })
        .await
    }

    async fn search(
        &self,
        session: SessionId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, StoreError> {
        let Some(query) = fts_query(query) else {
            return Ok(Vec::new());
        };
        self.read(move |conn| {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT e.seq, e.segment, bm25(events_fts),
                            snippet(events_fts, 0, '[', ']', '…', 12)
                     FROM events_fts JOIN events e ON e.id = events_fts.rowid
                     WHERE events_fts MATCH ?1 AND events_fts.session_id = ?2
                     ORDER BY bm25(events_fts) LIMIT ?3",
                )
                .map_err(backend)?;
            let rows = stmt
                .query_map(
                    params![
                        query,
                        session.to_string(),
                        i64::try_from(limit).unwrap_or(i64::MAX)
                    ],
                    |r| {
                        Ok(SearchHit {
                            seq: Seq(u64::try_from(r.get::<_, i64>(0)?).unwrap_or(0)),
                            segment: r.get(1)?,
                            // bm25 is lower-is-better; the port wants higher-is-better.
                            score: -r.get::<_, f64>(2)?,
                            snippet: r.get(3)?,
                        })
                    },
                )
                .map_err(backend)?;
            rows.map(|r| r.map_err(backend)).collect()
        })
        .await
    }
}

#[async_trait]
impl BlobStore for SqliteStore {
    async fn put(
        &self,
        bytes: Vec<u8>,
        media_type: &str,
        original_name: Option<&str>,
    ) -> Result<BlobRef, StoreError> {
        let dir = self.blobs.clone();
        let size = bytes.len() as u64;
        let blob = tokio::task::spawn_blocking(move || {
            let blob = blobs::address(&bytes);
            blobs::write(&dir, &blob, &bytes).map(|()| blob)
        })
        .await
        .map_err(|e| StoreError::Backend(format!("blob write failed: {e}")))??;
        // The file is in place before its row exists, so a row never points at nothing.
        let media_type = media_type.to_owned();
        let original_name = original_name.map(str::to_owned);
        self.writer
            .run(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO blobs (hash, media_type, bytes, original_name, provider_file_ids, created_at)
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
                    params![blob.to_hex(), media_type, to_i64(size), original_name, now_ms()],
                )
                .map_err(backend)?;
                Ok(())
            })
            .await?;
        Ok(blob)
    }

    async fn get(&self, blob: &BlobRef) -> Result<Vec<u8>, StoreError> {
        let dir = self.blobs.clone();
        let blob = *blob;
        tokio::task::spawn_blocking(move || blobs::read(&dir, &blob))
            .await
            .map_err(|e| StoreError::Backend(format!("blob read failed: {e}")))?
    }

    async fn meta(&self, blob: &BlobRef) -> Result<Option<BlobMeta>, StoreError> {
        let blob = *blob;
        self.read(move |conn| {
            conn.query_row(
                "SELECT media_type, bytes, original_name FROM blobs WHERE hash = ?1",
                [blob.to_hex()],
                |r| {
                    Ok(BlobMeta {
                        blob,
                        media_type: r.get(0)?,
                        bytes: u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                        original_name: r.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(backend)
        })
        .await
    }
}

const SESSION_COLUMNS: &str =
    "id, created_at, title, persona, cwd, status, parent_id, forked_at_seq, current_segment";

fn read_session(r: &rusqlite::Row<'_>) -> Result<SessionRecord, StoreError> {
    let get_text = |i: usize| r.get::<_, Option<String>>(i).map_err(backend);
    let id = get_text(0)?.unwrap_or_default();
    let status = get_text(5)?;
    let forked_at: Option<i64> = r.get(7).map_err(backend)?;
    Ok(SessionRecord {
        id: parse_id(&id, "session")?,
        created_at: r.get(1).map_err(backend)?,
        title: get_text(2)?,
        persona: get_text(3)?.unwrap_or_default(),
        cwd: PathBuf::from(get_text(4)?.unwrap_or_default()),
        status: if status.as_deref() == Some("closed") {
            SessionStatus::Closed
        } else {
            SessionStatus::Open
        },
        parent: get_text(6)?.map(|p| parse_id(&p, "session")).transpose()?,
        forked_at: forked_at.map(|f| Seq(u64::try_from(f).unwrap_or(0))),
        current_segment: r.get(8).map_err(backend)?,
    })
}

#[async_trait]
impl SessionStore for SqliteStore {
    async fn create_session(&self, record: SessionRecord) -> Result<(), StoreError> {
        self.writer
            .run(move |conn| {
                let inserted = conn
                    .execute(
                        "INSERT OR IGNORE INTO sessions (id, created_at, title, persona, cwd, status, parent_id, forked_at_seq, current_segment)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            record.id.to_string(),
                            record.created_at,
                            record.title,
                            record.persona,
                            record.cwd.to_string_lossy(),
                            match record.status {
                                SessionStatus::Open => "open",
                                SessionStatus::Closed => "closed",
                            },
                            record.parent.map(|p| p.to_string()),
                            record.forked_at.map(|f| to_i64(f.0)),
                            record.current_segment
                        ],
                    )
                    .map_err(backend)?;
                if inserted == 0 {
                    return Err(StoreError::Conflict(format!(
                        "session {} exists",
                        record.id
                    )));
                }
                Ok(())
            })
            .await
    }

    async fn session(&self, id: SessionId) -> Result<Option<SessionRecord>, StoreError> {
        self.read(move |conn| {
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"
                ))
                .map_err(backend)?;
            let mut rows = stmt.query([id.to_string()]).map_err(backend)?;
            match rows.next().map_err(backend)? {
                Some(row) => read_session(row).map(Some),
                None => Ok(None),
            }
        })
        .await
    }

    async fn sessions(&self) -> Result<Vec<SessionRecord>, StoreError> {
        self.read(move |conn| {
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {SESSION_COLUMNS} FROM sessions ORDER BY created_at DESC, id DESC"
                ))
                .map_err(backend)?;
            let mut rows = stmt.query([]).map_err(backend)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(backend)? {
                out.push(read_session(row)?);
            }
            Ok(out)
        })
        .await
    }

    async fn tasks(&self, session: Option<SessionId>) -> Result<Vec<TaskRecord>, StoreError> {
        self.read(move |conn| {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, session_id, persona, state, state_reason, created_at, updated_at
                     FROM tasks WHERE ?1 IS NULL OR session_id = ?1 ORDER BY created_at, id",
                )
                .map_err(backend)?;
            let mut rows = stmt
                .query([session.map(|s| s.to_string())])
                .map_err(backend)?;
            let mut out = Vec::new();
            while let Some(r) = rows.next().map_err(backend)? {
                let id: String = r.get(0).map_err(backend)?;
                let session: String = r.get(1).map_err(backend)?;
                let state: String = r.get(3).map_err(backend)?;
                out.push(TaskRecord {
                    id: parse_id(&id, "task")?,
                    session: parse_id(&session, "session")?,
                    persona: r.get(2).map_err(backend)?,
                    state: parse_state(&state)?,
                    reason: r.get(4).map_err(backend)?,
                    created_at: r.get(5).map_err(backend)?,
                    updated_at: r.get(6).map_err(backend)?,
                });
            }
            Ok(out)
        })
        .await
    }

    async fn request_control(&self, task: TaskId, control: TaskControl) -> Result<(), StoreError> {
        self.writer
            .run(move |conn| {
                let session: Option<String> = conn
                    .query_row(
                        "SELECT session_id FROM tasks WHERE id = ?1",
                        [task.to_string()],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(backend)?;
                let session = session.ok_or_else(|| StoreError::NotFound(format!("task {task}")))?;
                conn.execute(
                    "INSERT INTO task_controls (session_id, task_id, control, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![session, task.to_string(), control_name(control), now_ms()],
                )
                .map_err(backend)?;
                Ok(())
            })
            .await
    }

    async fn take_controls(
        &self,
        session: SessionId,
    ) -> Result<Vec<(TaskId, TaskControl)>, StoreError> {
        self.writer
            .run(move |conn| {
                let mut stmt = conn
                    .prepare_cached(
                        "SELECT id, task_id, control FROM task_controls WHERE session_id = ?1 ORDER BY id",
                    )
                    .map_err(backend)?;
                let rows: Vec<(i64, String, String)> = stmt
                    .query_map([session.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .map_err(backend)?
                    .collect::<Result<_, _>>()
                    .map_err(backend)?;
                let mut out = Vec::with_capacity(rows.len());
                for (id, task, control) in rows {
                    conn.execute("DELETE FROM task_controls WHERE id = ?1", [id])
                        .map_err(backend)?;
                    out.push((parse_id(&task, "task")?, parse_control(&control)?));
                }
                Ok(out)
            })
            .await
    }
}

#[async_trait]
impl SnapshotStore for SqliteStore {
    async fn record_snapshot(
        &self,
        session: SessionId,
        turn: Seq,
        snapshot: Snapshot,
    ) -> Result<(), StoreError> {
        self.writer
            .run(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO snapshots (session_id, seq, path, blob_hash) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        session.to_string(),
                        to_i64(turn.0),
                        snapshot.path.to_string_lossy(),
                        snapshot.before.map(|b| b.to_hex())
                    ],
                )
                .map_err(backend)?;
                Ok(())
            })
            .await
    }

    async fn undo_last_turn(&self, session: SessionId) -> Result<Option<Undone>, StoreError> {
        let rows: Vec<(i64, String, Option<String>)> = self
            .read(move |conn| {
                let mut stmt = conn
                    .prepare_cached(
                        "SELECT seq, path, blob_hash FROM snapshots
                         WHERE session_id = ?1 AND seq = (SELECT MAX(seq) FROM snapshots WHERE session_id = ?1)
                         ORDER BY path",
                    )
                    .map_err(backend)?;
                let rows = stmt
                    .query_map([session.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .map_err(backend)?
                    .collect::<Result<_, _>>()
                    .map_err(backend)?;
                Ok(rows)
            })
            .await?;
        let Some(turn) = rows.first().map(|r| r.0) else {
            return Ok(None);
        };
        let dir = self.blobs.clone();
        let restore = rows.clone();
        let paths = tokio::task::spawn_blocking(move || restore_files(&dir, &restore))
            .await
            .map_err(|e| StoreError::Backend(format!("undo failed: {e}")))??;
        // Forget the turn only after every file is back, so an interrupted
        // undo can simply be repeated.
        self.writer
            .run(move |conn| {
                conn.execute(
                    "DELETE FROM snapshots WHERE session_id = ?1 AND seq = ?2",
                    params![session.to_string(), turn],
                )
                .map_err(backend)?;
                Ok(())
            })
            .await?;
        Ok(Some(Undone {
            turn: Seq(u64::try_from(turn).unwrap_or(0)),
            paths,
        }))
    }
}

/// Puts each snapshot's contents back (or removes files the turn created).
fn restore_files(
    dir: &Path,
    rows: &[(i64, String, Option<String>)],
) -> Result<Vec<PathBuf>, StoreError> {
    let io = |path: &Path, e: std::io::Error| {
        StoreError::Backend(format!("cannot restore {}: {e}", path.display()))
    };
    let mut paths = Vec::with_capacity(rows.len());
    for (_, path, blob) in rows {
        let path = PathBuf::from(path);
        match blob {
            Some(hex) => {
                let blob: BlobRef = hex
                    .parse()
                    .map_err(|_| StoreError::Corrupt(format!("bad blob hash {hex:?}")))?;
                let bytes = blobs::read(dir, &blob)?;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| io(&path, e))?;
                }
                let tmp = path.with_extension("harness-undo.tmp");
                std::fs::write(&tmp, &bytes).map_err(|e| io(&path, e))?;
                std::fs::rename(&tmp, &path).map_err(|e| io(&path, e))?;
            }
            None => match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io(&path, e)),
            },
        }
        paths.push(path);
    }
    Ok(paths)
}
