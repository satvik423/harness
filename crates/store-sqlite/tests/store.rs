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
    assert_eq!(SCHEMA_VERSION, 3);
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

    // /diff sees each file's state before the session first touched it.
    let first = store.session_snapshots(id).await.unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(
        first.iter().find(|s| s.path == file).unwrap().before,
        Some(v1)
    );
    assert_eq!(
        first.iter().find(|s| s.path == created).unwrap().before,
        None
    );

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

fn blob_file(dir: &Path, blob: &harness_types::BlobRef) -> std::path::PathBuf {
    let hex = blob.to_hex();
    dir.join(".harness/blobs").join(&hex[..2]).join(&hex[2..])
}

/// A session with a task, a searchable reply, usage, a control request, a
/// frame blob of its own, a snapshot blob, and a reference to `shared`.
async fn busy_session(
    store: &SqliteStore,
    dir: &Path,
    own: &str,
    shared: harness_types::BlobRef,
) -> (SessionId, harness_types::BlobRef, harness_types::BlobRef) {
    let id = new_session(store, dir).await;
    let task = TaskId::new();
    let frame = store
        .put(own.as_bytes().to_vec(), "application/json", None)
        .await
        .unwrap();
    let before = store
        .put(format!("{own} before").into_bytes(), "text/plain", None)
        .await
        .unwrap();
    let events = vec![
        Event::SegmentOpened {
            segment: 0,
            opener: Message::user_text("opener"),
            frame,
        },
        Event::TaskState {
            task,
            from: TaskState::Queued,
            to: TaskState::Running,
            reason: "started".into(),
        },
        Event::AssistantMessage {
            task,
            message: Message::new(
                Role::Assistant,
                vec![ContentBlock::text(format!("{own} zebra"))],
            ),
        },
        Event::ToolFinished {
            call_id: "c1".into(),
            ok: true,
            sent: vec![ContentBlock::text("out")],
            full: BlobOrText::Blob { blob: shared },
        },
        Event::Usage {
            task,
            usage: Usage::default(),
            cost_micros: 1,
        },
    ];
    for (i, event) in events.into_iter().enumerate() {
        let seq = u64::try_from(i).unwrap() + 1;
        store
            .append(logged(id, seq, Some(task), event))
            .await
            .unwrap();
    }
    store
        .record_snapshot(
            id,
            Seq(3),
            Snapshot {
                path: dir.join(format!("{own}.txt")),
                before: Some(before),
            },
        )
        .await
        .unwrap();
    store
        .request_control(task, TaskControl::Pause)
        .await
        .unwrap();
    (id, frame, before)
}

#[tokio::test]
async fn deleting_a_session_removes_its_rows_and_frees_unshared_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let shared = store
        .put(b"shared".to_vec(), "text/plain", None)
        .await
        .unwrap();
    let (gone, gone_frame, gone_before) = busy_session(&store, dir.path(), "alpha", shared).await;
    let (kept, kept_frame, _) = busy_session(&store, dir.path(), "beta", shared).await;

    let deleted = store.delete_session(gone).await.unwrap();

    assert_eq!(deleted.events, 5);
    assert_eq!(deleted.blobs_freed, 2, "its frame and its snapshot blob");
    assert!(store.session(gone).await.unwrap().is_none());
    assert_eq!(
        store
            .sessions()
            .await
            .unwrap()
            .iter()
            .map(|s| s.id)
            .collect::<Vec<_>>(),
        vec![kept]
    );
    assert!(
        store
            .range(gone, Seq::FIRST, Seq(u64::MAX))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.last_seq(gone).await.unwrap(), None);
    assert!(store.tasks(Some(gone)).await.unwrap().is_empty());
    assert!(store.session_snapshots(gone).await.unwrap().is_empty());
    assert!(store.take_controls(gone).await.unwrap().is_empty());
    assert!(store.search(gone, "zebra", 10).await.unwrap().is_empty());
    assert_eq!(store.search(kept, "zebra", 10).await.unwrap().len(), 1);
    assert!(!blob_file(dir.path(), &gone_frame).exists());
    assert!(!blob_file(dir.path(), &gone_before).exists());
    assert!(store.meta(&gone_frame).await.unwrap().is_none());
    // Still referenced by the other session.
    assert!(blob_file(dir.path(), &shared).exists());
    assert!(blob_file(dir.path(), &kept_frame).exists());
    assert_eq!(store.tasks(Some(kept)).await.unwrap().len(), 1);
    assert_eq!(store.take_controls(kept).await.unwrap().len(), 1);
}

#[tokio::test]
async fn sessions_that_are_open_or_unknown_are_not_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let shared = store
        .put(b"shared".to_vec(), "text/plain", None)
        .await
        .unwrap();
    let (id, _, _) = busy_session(&store, dir.path(), "alpha", shared).await;

    let lock = store.lock_session(id).unwrap();
    assert!(matches!(
        store.delete_session(id).await,
        Err(StoreError::Conflict(_))
    ));
    drop(lock);
    assert!(matches!(
        store.delete_session(SessionId::new()).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(store.session(id).await.unwrap().is_some());
}

#[tokio::test]
async fn blobs_stay_while_another_session_is_open() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let shared = store
        .put(b"shared".to_vec(), "text/plain", None)
        .await
        .unwrap();
    let (gone, gone_frame, _) = busy_session(&store, dir.path(), "alpha", shared).await;
    let (open, _, _) = busy_session(&store, dir.path(), "beta", shared).await;

    // The open session may be writing a blob with the same contents right now.
    let lock = store.lock_session(open).unwrap();
    let deleted = store.delete_session(gone).await.unwrap();
    assert_eq!(deleted.blobs_freed, 0);
    assert!(blob_file(dir.path(), &gone_frame).exists());
    drop(lock);

    // A later delete frees everything nothing references, the earlier leftovers too.
    let deleted = store.delete_session(open).await.unwrap();
    assert_eq!(
        deleted.blobs_freed, 5,
        "both frames, both snapshots, the shared blob"
    );
    assert!(!blob_file(dir.path(), &gone_frame).exists());
}

#[tokio::test]
async fn recent_inputs_come_from_every_session_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let (a, b) = (
        new_session(&store, dir.path()).await,
        new_session(&store, dir.path()).await,
    );
    let task = TaskId::new();
    let input = |text: &str| Event::UserInput {
        task,
        message: Message::user_text(text),
    };
    store
        .append(logged(a, 1, Some(task), input("first")))
        .await
        .unwrap();
    store
        .append(logged(b, 1, Some(task), input("second")))
        .await
        .unwrap();
    store
        .append(logged(b, 2, Some(task), started("c")))
        .await
        .unwrap();
    store
        .append(logged(a, 2, Some(task), input("third")))
        .await
        .unwrap();

    let texts: Vec<String> = store
        .recent_inputs(2)
        .await
        .unwrap()
        .iter()
        .map(Message::text)
        .collect();
    assert_eq!(texts, ["third", "second"]);
}

#[tokio::test]
async fn compactions_fill_the_anchor_and_pin_tables_and_close_the_segment() {
    use harness_protocol::{Anchor, CompactionRecord, PinKind, PinnedFact};
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let id = new_session(&store, dir.path()).await;
    let frame = store
        .put(b"{}".to_vec(), "application/json", None)
        .await
        .unwrap();
    store
        .append(logged(
            id,
            1,
            None,
            Event::SegmentOpened {
                segment: 0,
                opener: Message::user_text("env"),
                frame,
            },
        ))
        .await
        .unwrap();
    let pin = |id, superseded_by| PinnedFact {
        id,
        quote: format!("fact {id}"),
        seq: Seq(1),
        kind: PinKind::Id,
        superseded_by,
    };
    let compacted = |pins: Vec<PinnedFact>, anchor: &str, seq| {
        logged(
            id,
            seq,
            None,
            Event::Compacted {
                from_segment: 0,
                to_segment: 1,
                anchors: 1,
                before: 900,
                after: 90,
                record: Box::new(CompactionRecord {
                    segment: 0,
                    from: Seq(1),
                    to: Seq(seq - 1),
                    goal: "g".into(),
                    state_of_work: "s".into(),
                    decisions: vec![],
                    pins,
                    files_touched: vec![],
                    open_questions: vec![],
                    next_steps: vec![],
                    anchors: vec![Anchor {
                        id: anchor.into(),
                        segment: 0,
                        from: Seq(1),
                        to: Seq(seq - 1),
                        topic: "t".into(),
                        has: vec!["error text".into()],
                        tokens: 42,
                    }],
                }),
            },
        )
    };
    store
        .append(compacted(vec![pin(1, None)], "a1", 2))
        .await
        .unwrap();
    store
        .append(compacted(vec![pin(1, Some(Seq(2))), pin(2, None)], "a2", 3))
        .await
        .unwrap();
    let db = rusqlite::Connection::open(dir.path().join(".harness/state.db")).unwrap();
    let pins: Vec<(i64, bool)> = db
        .prepare("SELECT pin_id, active FROM pinned_facts WHERE session_id = ?1 ORDER BY pin_id")
        .unwrap()
        .query_map([id.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(pins, [(1, false), (2, true)]);
    let anchors: Vec<(String, String)> = db
        .prepare("SELECT anchor_id, has_json FROM anchors WHERE session_id = ?1 ORDER BY anchor_id")
        .unwrap()
        .query_map([id.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(anchors[0], ("a1".to_owned(), "[\"error text\"]".to_owned()));
    assert_eq!(anchors.len(), 2);
    let (end, before): (i64, i64) = db
        .query_row(
            "SELECT end_seq, tokens_before FROM segments WHERE session_id = ?1 AND segment = 0",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((end, before), (2, 900));
    // Deleting the session removes the projections too.
    store.delete_session(id).await.unwrap();
    let left: i64 = db
        .query_row("SELECT COUNT(*) FROM pinned_facts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_fork_reads_its_parent_up_to_the_fork_point_and_continues_numbering_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path()).unwrap();
    let parent = new_session(&store, dir.path()).await;
    let task = TaskId::new();
    let say = |text: &str| Event::UserInput {
        task,
        message: Message::user_text(text),
    };
    for (i, word) in ["alpha", "bravo", "charlie", "delta", "echo"]
        .iter()
        .enumerate()
    {
        store
            .append(logged(
                parent,
                i as u64 + 1,
                Some(task),
                say(&format!("parent says {word}")),
            ))
            .await
            .unwrap();
    }
    let fork = |id, from: SessionId, at| SessionRecord {
        parent: Some(from),
        forked_at: Some(Seq(at)),
        ..record(id, dir.path())
    };
    let child = SessionId::new();
    store.create_session(fork(child, parent, 3)).await.unwrap();
    assert_eq!(store.last_seq(child).await.unwrap(), Some(Seq(3)));
    assert!(matches!(
        store.append(logged(child, 1, None, say("too early"))).await,
        Err(StoreError::Conflict(_))
    ));
    store
        .append(logged(child, 4, Some(task), say("child says foxtrot")))
        .await
        .unwrap();

    let seen: Vec<(u64, SessionId)> = store
        .range(child, Seq::FIRST, Seq(u64::MAX))
        .await
        .unwrap()
        .iter()
        .map(|e| (e.envelope.seq.0, e.envelope.session))
        .collect();
    assert_eq!(seen, [(1, parent), (2, parent), (3, parent), (4, child)]);
    assert_eq!(store.range(child, Seq(3), Seq(4)).await.unwrap().len(), 2);

    let hits = |q: &'static str, id| {
        let store = &store;
        async move {
            store
                .search(id, q, 10)
                .await
                .unwrap()
                .iter()
                .map(|h| h.seq.0)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(hits("charlie", child).await, [3]);
    assert!(
        hits("delta", child).await.is_empty(),
        "after the fork point"
    );
    assert_eq!(hits("foxtrot", child).await, [4]);
    assert!(hits("foxtrot", parent).await.is_empty());

    // A fork of the fork reads through both.
    let grandchild = SessionId::new();
    store
        .create_session(fork(grandchild, child, 4))
        .await
        .unwrap();
    let seqs: Vec<u64> = store
        .range(grandchild, Seq::FIRST, Seq(u64::MAX))
        .await
        .unwrap()
        .iter()
        .map(|e| e.envelope.seq.0)
        .collect();
    assert_eq!(seqs, [1, 2, 3, 4]);
    assert!(matches!(
        store.delete_session(child).await,
        Err(StoreError::Conflict(_))
    ));
}
