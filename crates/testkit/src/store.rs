//! [`MemoryStore`]: in-memory storage ports for tests.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use harness_ports::{
    BlobMeta, BlobStore, EventLog, LoggedEvent, SearchHit, SessionRecord, SessionStore, Snapshot,
    SnapshotStore, StoreError, TaskControl, TaskRecord, Undone,
};
use harness_protocol::Event;
use harness_types::{BlobRef, Seq, SessionId, TaskId};

#[derive(Default)]
struct Inner {
    sessions: Vec<SessionRecord>,
    events: HashMap<SessionId, Vec<LoggedEvent>>,
    tasks: Vec<TaskRecord>,
    controls: Vec<(SessionId, TaskId, TaskControl)>,
    blobs: HashMap<BlobRef, (Vec<u8>, BlobMeta)>,
    /// session → turn → path → snapshot (first one kept).
    snapshots: HashMap<SessionId, BTreeMap<Seq, BTreeMap<String, Snapshot>>>,
}

/// Implements [`EventLog`], [`SessionStore`], [`SnapshotStore`] and
/// [`BlobStore`] in memory, with the same rules as the SQLite store: gapless
/// sequence numbers, events only for known sessions, deltas refused, and a
/// task view kept up to date from `TaskState` events. Undo really restores files.
///
/// # Examples
/// ```
/// # tokio_test_block_on(async {
/// use harness_ports::{EventLog, LoggedEvent, SessionRecord, SessionStatus, SessionStore};
/// use harness_protocol::{Envelope, Event};
/// use harness_testkit::MemoryStore;
/// use harness_types::{Seq, SessionId};
///
/// let store = MemoryStore::default();
/// let id = SessionId::new();
/// store.create_session(SessionRecord {
///     id, created_at: 0, title: None, persona: "default".into(), cwd: "/p".into(),
///     status: SessionStatus::Open, parent: None, forked_at: None, current_segment: 0,
/// }).await.unwrap();
/// let event = |seq| LoggedEvent {
///     envelope: Envelope { seq: Seq(seq), ts: 0, session: id, event: Event::ToolStarted { call_id: "c".into() } },
///     task: None, segment: 0,
/// };
/// store.append(event(1)).await.unwrap();
/// assert!(store.append(event(3)).await.is_err()); // gap
/// assert_eq!(store.last_seq(id).await.unwrap(), Some(Seq(1)));
/// # });
/// # fn tokio_test_block_on<F: std::future::Future>(f: F) -> F::Output {
/// #     tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
/// # }
/// ```
#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStore").finish_non_exhaustive()
    }
}

impl MemoryStore {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every event of a session, in order (for assertions).
    #[must_use]
    pub fn events(&self, session: SessionId) -> Vec<LoggedEvent> {
        self.lock()
            .events
            .get(&session)
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl EventLog for MemoryStore {
    async fn append(&self, event: LoggedEvent) -> Result<(), StoreError> {
        let kind = event.envelope.event.kind();
        if !event.envelope.event.is_persisted() {
            return Err(StoreError::NotPersistable(kind));
        }
        let mut inner = self.lock();
        let session = event.envelope.session;
        let Some(record) = inner.sessions.iter().find(|s| s.id == session).cloned() else {
            return Err(StoreError::NotFound(format!("session {session}")));
        };
        let events = inner.events.entry(session).or_default();
        let expected = events.last().map_or(Seq::FIRST, |e| e.envelope.seq.next());
        if event.envelope.seq != expected {
            return Err(StoreError::Conflict(format!(
                "expected sequence number {}, got {}",
                expected.0, event.envelope.seq.0
            )));
        }
        let ts = event.envelope.ts;
        if let Event::TaskState {
            task, to, reason, ..
        } = &event.envelope.event
        {
            match inner.tasks.iter_mut().find(|t| t.id == *task) {
                Some(t) => {
                    t.state = *to;
                    t.reason = Some(reason.clone());
                    t.updated_at = ts;
                }
                None => inner.tasks.push(TaskRecord {
                    id: *task,
                    session,
                    persona: record.persona.clone(),
                    state: *to,
                    reason: Some(reason.clone()),
                    created_at: ts,
                    updated_at: ts,
                }),
            }
        }
        if let Event::SegmentOpened { segment, .. } = &event.envelope.event
            && let Some(s) = inner.sessions.iter_mut().find(|s| s.id == session)
        {
            s.current_segment = *segment;
        }
        inner.events.entry(session).or_default().push(event);
        Ok(())
    }

    async fn last_seq(&self, session: SessionId) -> Result<Option<Seq>, StoreError> {
        Ok(self
            .lock()
            .events
            .get(&session)
            .and_then(|e| e.last())
            .map(|e| e.envelope.seq))
    }

    async fn range(
        &self,
        session: SessionId,
        from: Seq,
        to: Seq,
    ) -> Result<Vec<LoggedEvent>, StoreError> {
        Ok(self
            .events(session)
            .into_iter()
            .filter(|e| e.envelope.seq >= from && e.envelope.seq <= to)
            .collect())
    }

    async fn search(
        &self,
        session: SessionId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, StoreError> {
        let needle = query.to_lowercase();
        Ok(self
            .events(session)
            .into_iter()
            .filter_map(|e| {
                let text = serde_json::to_string(&e.envelope.event)
                    .ok()?
                    .to_lowercase();
                text.contains(&needle).then(|| SearchHit {
                    seq: e.envelope.seq,
                    segment: e.segment,
                    score: 1.0,
                    snippet: query.to_owned(),
                })
            })
            .take(limit)
            .collect())
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn create_session(&self, record: SessionRecord) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.sessions.iter().any(|s| s.id == record.id) {
            return Err(StoreError::Conflict(format!(
                "session {} exists",
                record.id
            )));
        }
        inner.sessions.push(record);
        Ok(())
    }

    async fn session(&self, id: SessionId) -> Result<Option<SessionRecord>, StoreError> {
        Ok(self.lock().sessions.iter().find(|s| s.id == id).cloned())
    }

    async fn sessions(&self) -> Result<Vec<SessionRecord>, StoreError> {
        let mut all = self.lock().sessions.clone();
        all.reverse();
        Ok(all)
    }

    async fn tasks(&self, session: Option<SessionId>) -> Result<Vec<TaskRecord>, StoreError> {
        Ok(self
            .lock()
            .tasks
            .iter()
            .filter(|t| session.is_none_or(|s| t.session == s))
            .cloned()
            .collect())
    }

    async fn request_control(&self, task: TaskId, control: TaskControl) -> Result<(), StoreError> {
        let mut inner = self.lock();
        let Some(session) = inner.tasks.iter().find(|t| t.id == task).map(|t| t.session) else {
            return Err(StoreError::NotFound(format!("task {task}")));
        };
        inner.controls.push((session, task, control));
        Ok(())
    }

    async fn take_controls(
        &self,
        session: SessionId,
    ) -> Result<Vec<(TaskId, TaskControl)>, StoreError> {
        let mut inner = self.lock();
        let (mine, rest): (Vec<_>, Vec<_>) = inner
            .controls
            .drain(..)
            .partition(|(s, _, _)| *s == session);
        inner.controls = rest;
        Ok(mine.into_iter().map(|(_, t, c)| (t, c)).collect())
    }
}

#[async_trait]
impl SnapshotStore for MemoryStore {
    async fn record_snapshot(
        &self,
        session: SessionId,
        turn: Seq,
        snapshot: Snapshot,
    ) -> Result<(), StoreError> {
        self.lock()
            .snapshots
            .entry(session)
            .or_default()
            .entry(turn)
            .or_default()
            .entry(snapshot.path.to_string_lossy().into_owned())
            .or_insert(snapshot);
        Ok(())
    }

    async fn undo_last_turn(&self, session: SessionId) -> Result<Option<Undone>, StoreError> {
        let (turn, snapshots, blobs) = {
            let inner = self.lock();
            let Some((turn, files)) = inner
                .snapshots
                .get(&session)
                .and_then(|t| t.last_key_value())
            else {
                return Ok(None);
            };
            let blobs: HashMap<BlobRef, Vec<u8>> = inner
                .blobs
                .iter()
                .map(|(k, (v, _))| (*k, v.clone()))
                .collect();
            (*turn, files.values().cloned().collect::<Vec<_>>(), blobs)
        };
        let mut paths = Vec::new();
        for snap in snapshots {
            let io = |e: std::io::Error| StoreError::Backend(e.to_string());
            match snap.before {
                Some(blob) => {
                    let bytes = blobs
                        .get(&blob)
                        .ok_or_else(|| StoreError::NotFound(format!("blob {blob}")))?;
                    if let Some(parent) = snap.path.parent() {
                        std::fs::create_dir_all(parent).map_err(io)?;
                    }
                    std::fs::write(&snap.path, bytes).map_err(io)?;
                }
                None => match std::fs::remove_file(&snap.path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io(e)),
                },
            }
            paths.push(snap.path);
        }
        if let Some(turns) = self.lock().snapshots.get_mut(&session) {
            turns.remove(&turn);
        }
        Ok(Some(Undone { turn, paths }))
    }
}

#[async_trait]
impl BlobStore for MemoryStore {
    async fn put(
        &self,
        bytes: Vec<u8>,
        media_type: &str,
        original_name: Option<&str>,
    ) -> Result<BlobRef, StoreError> {
        let blob = BlobRef::from_bytes(*blake3::hash(&bytes).as_bytes());
        let meta = BlobMeta {
            blob,
            media_type: media_type.to_owned(),
            bytes: bytes.len() as u64,
            original_name: original_name.map(str::to_owned),
        };
        self.lock().blobs.entry(blob).or_insert((bytes, meta));
        Ok(blob)
    }

    async fn get(&self, blob: &BlobRef) -> Result<Vec<u8>, StoreError> {
        self.lock()
            .blobs
            .get(blob)
            .map(|(b, _)| b.clone())
            .ok_or_else(|| StoreError::NotFound(format!("blob {blob}")))
    }

    async fn meta(&self, blob: &BlobRef) -> Result<Option<BlobMeta>, StoreError> {
        Ok(self.lock().blobs.get(blob).map(|(_, m)| m.clone()))
    }
}
