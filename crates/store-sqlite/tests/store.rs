//! Behaviour of the SQLite store through its ports (P3-T1, P3-T2).

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;

use harness_ports::{
    BlobStore, EventLog, LoggedEvent, SessionRecord, SessionStatus, SessionStore, Snapshot,
    SnapshotStore, StoreError, TaskControl,
};
use harness_protocol::{BlobOrText, Envelope, Event, TaskState};
use harness_store_sqlite::{SCHEMA_VERSION, SqliteStore};
use harness_types::{ContentBlock, Message, Role, Seq, SessionId, TaskId, Usage};

fn record(id: SessionId, dir: &Path) -> SessionRecord {
    SessionRecord {
        id,
        created_at: 1_000,
        title: None,
        persona: "coder".into(),
        cwd: dir.to_path_buf(),
        status: SessionStatus::Open,
        parent: None,
        forked_at: None,
        current_segment: 0,
    }
}

fn logged(session: SessionId, seq: u64, task: Option<TaskId>, event: Event) -> LoggedEvent {
    LoggedEvent {
        envelope: Envelope {
            seq: Seq(seq),
            ts: 1_000 + i64::try_from(seq).unwrap(),
            session,
            event,
        },
        task,
        segment: 0,
    }
}

async fn new_session(store: &SqliteStore, dir: &Path) -> SessionId {
    let id = SessionId::new();
    store.create_session(record(id, dir)).await.unwrap();
    id
}

fn started(call: &str) -> Event {
    Event::ToolStarted {
        call_id: call.into(),
    }
}

#[tokio::test]
async fn opening_creates_the_layout_and_reopening_keeps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let store = SqliteStore::open(dir.path()).unwrap();
        let id = new_session(&store, dir.path()).await;
        store
            .append(logged(id, 1, None, started("c1")))
            .await
            .unwrap();
        id
    };
    let harness = dir.path().join(".harness");
    assert!(harness.join("state.db").exists());
    assert!(harness.join("blobs").is_dir());
    let gitignore = std::fs::read_to_string(harness.join(".gitignore")).unwrap();
    for entry in ["state.db*", "blobs/", "exports/"] {
        assert!(gitignore.contains(entry), "{gitignore}");
    }
    let store = SqliteStore::open(dir.path()).unwrap();
    assert_eq!(store.last_seq(id).await.unwrap(), Some(Seq(1)));
    assert_eq!(SCHEMA_VERSION, 2);
}

#[tokio::test]
async fn an_acknowledged_append_is_visible_to_a_second_connection() {
    let dir = tempfile::tempdir().unwrap();
    let a = SqliteStore::open(dir.path()).unwrap();
    let b = SqliteStore::open(dir.path()).unwrap();
    let id = new_session(&a, dir.path()).await;
    a.append(logged(id, 1, None, started("c1"))).await.unwrap();
    // NFR-03: once append returns, the event is committed.
    let seen = b.range(id, Seq(1), Seq(1)).await.unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].envelope.event, started("c1"));
}

#[tokio::test]
async fn sequence_numbers_must_be_gapless() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let id = new_session(&store, dir.path()).await;
    assert!(matches!(
        store.append(logged(id, 2, None, started("x"))).await,
        Err(StoreError::Conflict(_))
    ));
    store
        .append(logged(id, 1, None, started("a")))
        .await
        .unwrap();
    assert!(matches!(
        store.append(logged(id, 1, None, started("dup"))).await,
        Err(StoreError::Conflict(_))
    ));
    store
        .append(logged(id, 2, None, started("b")))
        .await
        .unwrap();
    assert_eq!(store.last_seq(id).await.unwrap(), Some(Seq(2)));
}

#[tokio::test]
async fn deltas_and_unknown_sessions_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let id = new_session(&store, dir.path()).await;
    let delta = Event::TextDelta {
        task: TaskId::new(),
        text: "x".into(),
    };
    assert!(matches!(
        store.append(logged(id, 1, None, delta)).await,
        Err(StoreError::NotPersistable("text_delta"))
    ));
    assert!(matches!(
        store
            .append(logged(SessionId::new(), 1, None, started("x")))
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.create_session(record(id, dir.path())).await,
        Err(StoreError::Conflict(_))
    ));
}

#[tokio::test]
async fn range_returns_events_in_order_with_routing_data() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let id = new_session(&store, dir.path()).await;
    let task = TaskId::new();
    for n in 1..=5 {
        store
            .append(logged(id, n, Some(task), started(&format!("c{n}"))))
            .await
            .unwrap();
    }
    let middle = store.range(id, Seq(2), Seq(4)).await.unwrap();
    let seqs: Vec<u64> = middle.iter().map(|e| e.envelope.seq.0).collect();
    assert_eq!(seqs, [2, 3, 4]);
    assert_eq!(middle[0].task, Some(task));
    assert_eq!(middle[0].envelope.ts, 1_002);
    assert_eq!(middle[0].envelope.session, id);
}

#[tokio::test]
async fn task_states_titles_and_segments_update_the_views() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let id = new_session(&store, dir.path()).await;
    let task = TaskId::new();
    let state = |from, to, reason: &str| Event::TaskState {
        task,
        from,
        to,
        reason: reason.into(),
    };
    store
        .append(logged(
            id,
            1,
            Some(task),
            state(TaskState::Queued, TaskState::Running, "started"),
        ))
        .await
        .unwrap();
    let input = Message::new(
        Role::User,
        vec![
            ContentBlock::text("<environment>…</environment>"),
            ContentBlock::text("fix the login test"),
        ],
    );
    store
        .append(logged(
            id,
            2,
            Some(task),
            Event::UserInput {
                task,
                message: input,
            },
        ))
        .await
        .unwrap();
    store
        .append(logged(
            id,
            3,
            Some(task),
            state(TaskState::Running, TaskState::Paused, "paused"),
        ))
        .await
        .unwrap();
    store
        .append(logged(
            id,
            4,
            None,
            Event::SegmentOpened {
                segment: 1,
                opener: Message::user_text("opener"),
                frame: harness_types::BlobRef::from_bytes([1; 32]),
            },
        ))
        .await
        .unwrap();

    let tasks = store.tasks(Some(id)).await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].state, TaskState::Paused);
    assert_eq!(tasks[0].reason.as_deref(), Some("paused"));
    assert_eq!((tasks[0].created_at, tasks[0].updated_at), (1_001, 1_003));
    assert_eq!(tasks[0].persona, "coder");
    let session = store.session(id).await.unwrap().unwrap();
    assert_eq!(session.title.as_deref(), Some("fix the login test"));
    assert_eq!(session.current_segment, 1);
    assert!(
        store
            .tasks(Some(SessionId::new()))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn usage_rows_count_turns() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let id = new_session(&store, dir.path()).await;
    let task = TaskId::new();
    for n in 1..=2 {
        store
            .append(logged(
                id,
                n,
                Some(task),
                Event::Usage {
                    task,
                    usage: Usage::default(),
                    cost_micros: 5,
                },
            ))
            .await
            .unwrap();
    }
    let db = rusqlite::Connection::open(dir.path().join(".harness/state.db")).unwrap();
    let turns: Vec<i64> = db
        .prepare("SELECT turn FROM usage WHERE task_id = ?1 ORDER BY turn")
        .unwrap()
        .query_map([task.to_string()], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(turns, [1, 2]);
}

#[tokio::test]
async fn sessions_are_listed_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let old = SessionId::new();
    let new = SessionId::new();
    store.create_session(record(old, dir.path())).await.unwrap();
    store
        .create_session(SessionRecord {
            created_at: 2_000,
            ..record(new, dir.path())
        })
        .await
        .unwrap();
    let ids: Vec<SessionId> = store
        .sessions()
        .await
        .unwrap()
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, [new, old]);
    assert!(store.session(SessionId::new()).await.unwrap().is_none());
}

#[tokio::test]
async fn search_finds_words_in_text_and_tool_output() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let id = new_session(&store, dir.path()).await;
    let task = TaskId::new();
    store
        .append(logged(
            id,
            1,
            Some(task),
            Event::UserInput {
                task,
                message: Message::user_text("please run the migrations"),
            },
        ))
        .await
        .unwrap();
    store
        .append(logged(
            id,
            2,
            Some(task),
            Event::ToolFinished {
                call_id: "c1".into(),
                ok: false,
                full: BlobOrText::Text {
                    text: "error: relation \"users\" already exists".into(),
                },
                sent: vec![],
            },
        ))
        .await
        .unwrap();
    let hits = store.search(id, "relation exists", 5).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].seq, Seq(2));
    assert!(
        hits[0].snippet.contains("[relation]"),
        "{}",
        hits[0].snippet
    );
    // Stemming: "migration" finds "migrations".
    assert_eq!(
        store.search(id, "migration", 5).await.unwrap()[0].seq,
        Seq(1)
    );
    // Query syntax cannot break the search.
    assert!(store.search(id, "AND ( \" OR", 5).await.unwrap().is_empty());
    assert!(
        store
            .search(SessionId::new(), "relation", 5)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn blobs_are_stored_once_in_sharded_folders_and_checked_on_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let blob = store
        .put(b"hello".to_vec(), "text/plain", Some("a.txt"))
        .await
        .unwrap();
    assert_eq!(
        store
            .put(b"hello".to_vec(), "text/plain", None)
            .await
            .unwrap(),
        blob
    );
    let hex = blob.to_hex();
    let path = dir
        .path()
        .join(".harness/blobs")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert!(path.exists());
    assert_eq!(store.get(&blob).await.unwrap(), b"hello");
    let meta = store.meta(&blob).await.unwrap().unwrap();
    assert_eq!(
        (meta.bytes, meta.original_name.as_deref()),
        (5, Some("a.txt"))
    );
    std::fs::write(&path, b"tampered").unwrap();
    assert!(matches!(
        store.get(&blob).await,
        Err(StoreError::Corrupt(_))
    ));
    let missing = harness_types::BlobRef::from_bytes([9; 32]);
    assert!(matches!(
        store.get(&missing).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(store.meta(&missing).await.unwrap().is_none());
}

#[tokio::test]
async fn undo_restores_the_last_turn_then_the_one_before() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let id = new_session(&store, dir.path()).await;
    let file = dir.path().join("a.txt");
    let created = dir.path().join("new/b.txt");

    // Turn 10: a.txt was "v1" before. Turn 20: a.txt was "v2", b.txt did not exist.
    let v1 = store.put(b"v1".to_vec(), "text/plain", None).await.unwrap();
    let v2 = store.put(b"v2".to_vec(), "text/plain", None).await.unwrap();
    let snap = |path: &Path, before| Snapshot {
        path: path.to_path_buf(),
        before,
    };
    store
        .record_snapshot(id, Seq(10), snap(&file, Some(v1)))
        .await
        .unwrap();
    store
        .record_snapshot(id, Seq(20), snap(&file, Some(v2)))
        .await
        .unwrap();
    // A second write in the same turn must not replace the "before" state.
    store
        .record_snapshot(id, Seq(20), snap(&file, Some(v1)))
        .await
        .unwrap();
    store
        .record_snapshot(id, Seq(20), snap(&created, None))
        .await
        .unwrap();
    std::fs::write(&file, "v3").unwrap();
    std::fs::create_dir_all(created.parent().unwrap()).unwrap();
    std::fs::write(&created, "new").unwrap();

    let undone = store.undo_last_turn(id).await.unwrap().unwrap();
    assert_eq!(undone.turn, Seq(20));
    assert_eq!(undone.paths.len(), 2);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "v2");
    assert!(!created.exists());

    let undone = store.undo_last_turn(id).await.unwrap().unwrap();
    assert_eq!(undone.turn, Seq(10));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "v1");
    assert!(store.undo_last_turn(id).await.unwrap().is_none());
}

#[tokio::test]
async fn control_requests_are_delivered_once_per_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let id = new_session(&store, dir.path()).await;
    let task = TaskId::new();
    store
        .append(logged(
            id,
            1,
            Some(task),
            Event::TaskState {
                task,
                from: TaskState::Queued,
                to: TaskState::Running,
                reason: "started".into(),
            },
        ))
        .await
        .unwrap();
    // A second process writes the request; the running one takes it.
    let other = SqliteStore::open(dir.path()).unwrap();
    other
        .request_control(task, TaskControl::Pause)
        .await
        .unwrap();
    other
        .request_control(task, TaskControl::Cancel)
        .await
        .unwrap();
    assert!(matches!(
        other
            .request_control(TaskId::new(), TaskControl::Pause)
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        store.take_controls(id).await.unwrap(),
        [(task, TaskControl::Pause), (task, TaskControl::Cancel)]
    );
    assert!(store.take_controls(id).await.unwrap().is_empty());
}

#[tokio::test]
async fn session_locks_are_exclusive_across_store_handles() {
    let dir = tempfile::tempdir().unwrap();
    let a = SqliteStore::open(dir.path()).unwrap();
    let b = SqliteStore::open(dir.path()).unwrap();
    let id = SessionId::new();
    let lock = a.lock_session(id).unwrap();
    assert!(b.is_session_locked(id));
    drop(lock);
    assert!(!b.is_session_locked(id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_to_many_sessions_all_commit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(dir.path()).unwrap());
    let mut sessions = Vec::new();
    for _ in 0..8 {
        sessions.push(new_session(&store, dir.path()).await);
    }
    let mut handles = Vec::new();
    for id in sessions.clone() {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            for n in 1..=50 {
                store
                    .append(logged(id, n, None, started("c")))
                    .await
                    .unwrap();
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    for id in sessions {
        assert_eq!(store.last_seq(id).await.unwrap(), Some(Seq(50)));
    }
}
