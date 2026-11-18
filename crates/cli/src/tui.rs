//! [`CliBackend`]: the TUI's [`Backend`] over the runtime and the SQLite store.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use harness_policy::ApprovalPolicy;
use harness_ports::{
    BlobStore, Effort, EventLog, Provider, SessionStore, SnapshotStore, TaskRecord,
};
use harness_protocol::Op;
use harness_runtime::{ApprovalMode, SessionConfig, SessionHandle, resume_session, spawn_session};
use harness_store_sqlite::SessionLock;
use harness_tui::app::PROMPT_HISTORY;
use harness_tui::{Attached, Backend, ChangedFile, SessionSummary, SessionView};
use harness_types::{ContentBlock, ModelRef, Seq, SessionId};
use tokio::sync::Mutex;

use crate::factory::SessionFactory;
use crate::secrets::SystemSecrets;
use crate::sessions::recent_sessions;
use crate::wiring::{build_provider, session_frame};

/// Maximum model calls per task in the TUI.
const TUI_MAX_TURNS: u32 = 100;
/// How long leaving a session waits for its actor to stop.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

/// Builds the provider for a model. It may block (the OS keyring), so the
/// backend calls it on a blocking thread.
pub type Providers = dyn Fn(&ModelRef) -> Result<Arc<dyn Provider>, String> + Send + Sync;

/// The session the TUI is attached to, and its lock.
struct Current {
    handle: SessionHandle,
    id: SessionId,
    _lock: SessionLock,
}

/// Implements the TUI's [`Backend`].
pub struct CliBackend {
    factory: Arc<SessionFactory>,
    providers: Arc<Providers>,
    current: Mutex<Option<Current>>,
}

impl CliBackend {
    /// A backend with no session yet; call [`CliBackend::open`] next.
    /// Providers are built from the config with keys from the keyring or
    /// environment.
    #[must_use]
    pub fn new(factory: Arc<SessionFactory>) -> Self {
        let config = factory.config.clone();
        Self::with_providers(
            factory,
            Arc::new(move |model: &ModelRef| {
                build_provider(model, &config, &SystemSecrets).map_err(|e| e.to_string())
            }),
        )
    }

    /// Like [`CliBackend::new`], with providers built by `providers` (tests).
    #[must_use]
    pub fn with_providers(factory: Arc<SessionFactory>, providers: Arc<Providers>) -> Self {
        Self {
            factory,
            providers,
            current: Mutex::new(None),
        }
    }

    /// The provider for `model`, built off the async workers.
    async fn provider(&self, model: &ModelRef) -> Result<Arc<dyn Provider>, String> {
        let providers = Arc::clone(&self.providers);
        let model = model.clone();
        tokio::task::spawn_blocking(move || providers(&model))
            .await
            .map_err(|e| e.to_string())?
    }

    /// Stops a session the TUI is leaving. One that nobody typed into is
    /// deleted, so opening and closing harness leaves no empty sessions behind.
    async fn retire(&self, previous: Current) {
        let _ = previous.handle.send(Op::Shutdown).await;
        // Nothing may be written after the deletion below.
        let _ = tokio::time::timeout(SHUTDOWN_WAIT, previous.handle.closed()).await;
        let id = previous.id;
        drop(previous); // releases the session lock
        let store = &self.factory.store;
        if let Ok(Some(record)) = store.session(id).await
            && record.title.is_none()
            && let Err(e) = store.delete_session(id).await
        {
            tracing::warn!(session = %id, error = %e, "could not delete an empty session");
        }
    }

    /// Opens a session (new when `session` is `None`, else resumed) with
    /// `provider` and makes it current.
    ///
    /// # Errors
    /// A message if the policy, the lock or the runtime fails.
    pub async fn open(
        &self,
        session: Option<SessionId>,
        provider: Arc<dyn Provider>,
        model: ModelRef,
        effort: Option<Effort>,
    ) -> Result<Attached, String> {
        let f = &self.factory;
        let (deps, approval) = f.deps(provider).map_err(|e| e.to_string())?;
        let mut persona = harness_context::builtin_default();
        persona.model = model.clone();
        if effort.is_some() {
            persona.effort = effort;
        }
        let config = SessionConfig {
            cwd: f.cwd.clone(),
            persona: persona.clone(),
            approval: match approval {
                ApprovalPolicy::OnRequest => ApprovalMode::Ask,
                ApprovalPolicy::Never => ApprovalMode::Never,
            },
            max_turns: TUI_MAX_TURNS,
            output_cap_tokens: f.config.context.output_cap_tokens,
            max_parallel_tools: usize::try_from(f.config.runtime.max_parallel_tools).unwrap_or(1),
            price: f.price(&model),
        };
        let (handle, events, history, lock) = match session {
            Some(id) => {
                let lock = f.store.lock_session(id).map_err(|e| e.to_string())?;
                // History first: the receiver below sees recovery events live.
                let history = f
                    .store
                    .range(id, Seq::FIRST, Seq(u64::MAX))
                    .await
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|e| e.envelope)
                    .collect();
                let (handle, events, _) = resume_session(deps, config, id)
                    .await
                    .map_err(|e| e.to_string())?;
                (handle, events, history, lock)
            }
            None => {
                let (handle, events) = spawn_session(deps, config)
                    .await
                    .map_err(|e| e.to_string())?;
                let lock = f
                    .store
                    .lock_session(handle.id())
                    .map_err(|e| e.to_string())?;
                (handle, events, Vec::new(), lock)
            }
        };
        let id = handle.id();
        let previous = self.current.lock().await.replace(Current {
            handle,
            id,
            _lock: lock,
        });
        if let Some(previous) = previous {
            self.retire(previous).await;
        }
        let prompts = self.prompts().await;
        Ok(Attached {
            prompts,
            view: SessionView {
                id,
                persona: persona.name,
                model: model.clone(),
                effort: persona.effort,
                profile: f.profile_name().to_owned(),
                context_limit: f.config.context.compact_at_tokens,
                priced: f.price(&model).is_some(),
                cwd: f.cwd.clone(),
            },
            events,
            history,
        })
    }

    /// The user's earlier prompts in this project, oldest first, without the
    /// parts harness added (environment, notes, attachment references).
    async fn prompts(&self) -> Vec<String> {
        let messages = match self.factory.store.recent_inputs(PROMPT_HISTORY).await {
            Ok(messages) => messages,
            Err(e) => {
                tracing::warn!(error = %e, "could not read the prompt history");
                return Vec::new();
            }
        };
        let mut out: Vec<String> = Vec::with_capacity(messages.len());
        for message in messages.iter().rev() {
            let text = message
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text }
                        if !["<environment>", "<system-note>", "<attachment "]
                            .iter()
                            .any(|p| text.starts_with(p)) =>
                    {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.trim().is_empty() && out.last() != Some(&text) {
                out.push(text);
            }
        }
        out
    }

    async fn current_id(&self) -> Result<SessionId, String> {
        self.current
            .lock()
            .await
            .as_ref()
            .map(|c| c.id)
            .ok_or_else(|| "no session is open".to_owned())
    }

    /// Shuts the current session down (on quit); deletes it if nobody typed into it.
    pub async fn close(&self) {
        let current = self.current.lock().await.take();
        if let Some(current) = current {
            self.retire(current).await;
        }
    }
}

#[async_trait]
impl Backend for CliBackend {
    async fn send(&self, op: Op) -> Result<(), String> {
        let current = self.current.lock().await;
        let current = current.as_ref().ok_or("no session is open")?;
        current.handle.send(op).await.map_err(|e| e.to_string())
    }

    async fn tasks(&self) -> Result<Vec<TaskRecord>, String> {
        let id = self.current_id().await?;
        self.factory
            .store
            .tasks(Some(id))
            .await
            .map_err(|e| e.to_string())
    }

    async fn changes(&self) -> Result<Vec<ChangedFile>, String> {
        let id = self.current_id().await?;
        let store = &self.factory.store;
        let snapshots = store
            .session_snapshots(id)
            .await
            .map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(snapshots.len());
        for snap in snapshots {
            let before = match snap.before {
                Some(blob) => {
                    let bytes = store.get(&blob).await.map_err(|e| e.to_string())?;
                    Some(String::from_utf8_lossy(&bytes).into_owned())
                }
                None => None,
            };
            let after = tokio::fs::read(&snap.path)
                .await
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned());
            out.push(ChangedFile {
                path: snap.path,
                before,
                after,
            });
        }
        Ok(out)
    }

    async fn new_session(
        &self,
        model: ModelRef,
        effort: Option<Effort>,
    ) -> Result<Attached, String> {
        let provider = self.provider(&model).await?;
        self.open(None, provider, model, effort).await
    }

    async fn sessions(&self) -> Result<Vec<SessionSummary>, String> {
        let store = &self.factory.store;
        Ok(recent_sessions(store.as_ref())
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|r| SessionSummary {
                id: r.record.id,
                title: r.record.title,
                last_active: r.last_active,
                state: r.last_task.map(|t| t.state),
            })
            .collect())
    }

    async fn open_session(&self, id: SessionId) -> Result<Attached, String> {
        let store = &self.factory.store;
        // A session keeps the model and effort its segment was opened with (ADR-032).
        let frame = session_frame(store.as_ref(), store.as_ref(), id)
            .await
            .map_err(|e| e.to_string())?;
        let (model, effort) = match frame {
            Some(frame) => (
                format!("{}/{}", frame.provider, frame.model)
                    .parse()
                    .map_err(|e| format!("{e}"))?,
                frame.effort,
            ),
            None => (self.factory.config.default_model.clone(), None),
        };
        let provider = self.provider(&model).await?;
        self.open(Some(id), provider, model, effort).await
    }

    fn models(&self) -> Vec<ModelRef> {
        self.factory.models()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_config::{Config, Paths};
    use harness_protocol::{Event, InputItem, TaskState};
    use harness_store_sqlite::SqliteStore;
    use harness_testkit::FakeProvider;
    use serde_json::json;

    fn factory(dir: &std::path::Path) -> Arc<SessionFactory> {
        Arc::new(SessionFactory {
            config: Config::default(),
            paths: Paths {
                user_config_dir: dir.join(".c"),
                user_data_dir: dir.join(".d"),
            },
            cwd: dir.to_path_buf(),
            trusted: false,
            approval: None,
            full_access: false,
            store: Arc::new(SqliteStore::open(dir).unwrap()),
        })
    }

    /// A backend whose providers are all `fake` (no API keys in tests).
    fn backend_with(dir: &std::path::Path, fake: FakeProvider) -> CliBackend {
        CliBackend::with_providers(
            factory(dir),
            Arc::new(move |_: &ModelRef| Ok(Arc::new(fake.clone()) as Arc<dyn Provider>)),
        )
    }

    fn backend(dir: &std::path::Path) -> CliBackend {
        CliBackend::new(Arc::new(SessionFactory {
            config: Config::default(),
            paths: Paths {
                user_config_dir: dir.join(".c"),
                user_data_dir: dir.join(".d"),
            },
            cwd: dir.to_path_buf(),
            trusted: false,
            approval: None,
            full_access: false,
            store: Arc::new(SqliteStore::open(dir).unwrap()),
        }))
    }

    async fn until_done(events: &mut tokio::sync::broadcast::Receiver<harness_protocol::Envelope>) {
        loop {
            let env = tokio::time::timeout(std::time::Duration::from_secs(10), events.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(
                env.event,
                Event::TaskState {
                    to: TaskState::Completed | TaskState::Failed,
                    ..
                }
            ) {
                return;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_opened_through_the_backend_runs_and_reports_tasks_and_changes() {
        let dir = tempfile::tempdir().unwrap();
        let backend = backend(dir.path());
        let fake = FakeProvider::new()
            .with_tool_call_turn(
                "c1",
                "write_file",
                &json!({"path": "notes.md", "content": "hi\n"}),
            )
            .with_text_turn("written");
        let model: ModelRef = "anthropic/claude-opus-5-5".parse().unwrap();
        let mut attached = backend
            .open(None, Arc::new(fake), model.clone(), Some(Effort::Low))
            .await
            .unwrap();
        assert_eq!(attached.view.model, model);
        assert_eq!(attached.view.effort, Some(Effort::Low));
        assert!(attached.view.priced, "opus has a default price");
        assert_eq!(attached.view.profile, "auto");
        backend
            .send(Op::Submit {
                session: attached.view.id,
                input: vec![InputItem::Text {
                    text: "write notes".into(),
                }],
            })
            .await
            .unwrap();
        until_done(&mut attached.events).await;
        let tasks = backend.tasks().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Completed);
        let changes = backend.changes().await.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].before, None);
        assert_eq!(changes[0].after.as_deref(), Some("hi\n"));
        assert_eq!(backend.models()[0], Config::default().default_model);
        backend.close().await;
        assert!(
            backend.send(Op::Shutdown).await.is_err(),
            "no session after close"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn switching_sessions_keeps_used_ones_and_drops_empty_ones() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeProvider::new().with_text_turn("hello");
        let backend = backend_with(dir.path(), fake.clone());
        let model: ModelRef = "openai/gpt-6-luna".parse().unwrap();
        let mut used = backend
            .open(None, Arc::new(fake), model.clone(), Some(Effort::Low))
            .await
            .unwrap();
        let used_id = used.view.id;
        backend
            .send(Op::Submit {
                session: used_id,
                input: vec![InputItem::Text {
                    text: "hi there".into(),
                }],
            })
            .await
            .unwrap();
        until_done(&mut used.events).await;

        // A new session nobody types into…
        let empty = backend.new_session(model, None).await.unwrap();
        let listed = backend.sessions().await.unwrap();
        assert_eq!(listed.len(), 1, "sessions without a prompt are not listed");
        assert_eq!(listed[0].id, used_id);
        assert_eq!(listed[0].title.as_deref(), Some("hi there"));
        assert_eq!(listed[0].state, Some(TaskState::Completed));

        // …is deleted when the TUI leaves it; the used one comes back as it was.
        let again = backend.open_session(used_id).await.unwrap();
        assert_eq!(again.view.id, used_id);
        assert_eq!(
            again.prompts,
            ["hi there"],
            "the prompt history, harness notes left out"
        );
        assert_eq!(
            again.view.effort,
            Some(Effort::Low),
            "effort from the stored frame"
        );
        assert!(
            again
                .history
                .iter()
                .any(|e| matches!(e.event, Event::AssistantMessage { .. }))
        );
        let store = &backend.factory.store;
        assert!(store.session(empty.view.id).await.unwrap().is_none());
        backend.close().await;
        assert!(
            store.session(used_id).await.unwrap().is_some(),
            "used sessions stay"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reopening_a_stored_session_brings_its_history() {
        let dir = tempfile::tempdir().unwrap();
        let first = backend(dir.path());
        let fake = FakeProvider::new().with_text_turn("hello");
        let model: ModelRef = "openai/not-a-priced-model".parse().unwrap();
        let mut attached = first
            .open(None, Arc::new(fake), model.clone(), None)
            .await
            .unwrap();
        let id = attached.view.id;
        first
            .send(Op::Submit {
                session: id,
                input: vec![InputItem::Text { text: "hi".into() }],
            })
            .await
            .unwrap();
        until_done(&mut attached.events).await;
        assert!(!attached.view.priced, "no price for this model");
        first.close().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let second = backend(dir.path());
        let again = second
            .open(Some(id), Arc::new(FakeProvider::new()), model, None)
            .await
            .unwrap();
        assert!(
            again
                .history
                .iter()
                .any(|e| matches!(e.event, Event::AssistantMessage { .. }))
        );
        // The session is locked by `second` now.
        assert!(
            first
                .open(
                    Some(id),
                    Arc::new(FakeProvider::new()),
                    "openai/x".parse().unwrap(),
                    None
                )
                .await
                .is_err()
        );
        second.close().await;
    }
}
