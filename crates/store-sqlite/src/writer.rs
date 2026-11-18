//! The writer thread: the only place that writes to the database.
//!
//! Jobs arrive on a bounded channel. The thread takes every job that is
//! waiting (up to [`MAX_BATCH`]), runs them in one transaction, each inside its
//! own savepoint, commits, and only then answers each caller. A caller that has
//! its answer therefore knows its write is durable (NFR-03). Running SQLite on
//! its own thread keeps it off the async workers (guardrail 16).

use std::thread::JoinHandle;

use harness_ports::StoreError;
use rusqlite::{Connection, Transaction};
use tokio::sync::{mpsc, oneshot};

use crate::backend;

/// Jobs committed together, at most.
const MAX_BATCH: usize = 64;
/// Jobs waiting for the writer, at most; senders wait when it is full.
const QUEUE: usize = 256;

/// Sends the job's answer once the batch's commit result is known.
type Finish = Box<dyn FnOnce(Result<(), StoreError>) + Send>;
/// A write, run inside the batch transaction.
type Job = Box<dyn FnOnce(&mut Transaction<'_>) -> Finish + Send>;

/// Handle to the writer thread. Dropping it finishes queued jobs and stops the thread.
pub(crate) struct Writer {
    jobs: Option<mpsc::Sender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl Writer {
    /// Starts the thread on `conn` (already configured and migrated).
    ///
    /// # Errors
    /// A backend error if the thread cannot be spawned.
    pub(crate) fn start(mut conn: Connection) -> Result<Self, StoreError> {
        let (tx, mut rx) = mpsc::channel::<Job>(QUEUE);
        let thread = std::thread::Builder::new()
            .name("harness-sqlite-writer".into())
            .spawn(move || {
                while let Some(first) = rx.blocking_recv() {
                    let mut batch = vec![first];
                    while batch.len() < MAX_BATCH {
                        match rx.try_recv() {
                            Ok(job) => batch.push(job),
                            Err(_) => break,
                        }
                    }
                    run_batch(&mut conn, batch);
                }
            })
            .map_err(|e| StoreError::Backend(format!("cannot start the writer thread: {e}")))?;
        Ok(Self {
            jobs: Some(tx),
            thread: Some(thread),
        })
    }

    /// Runs `f` in the writer's transaction and returns its result after the commit.
    ///
    /// # Errors
    /// The error from `f` (its changes are rolled back, other jobs in the batch
    /// are unaffected), a commit failure, or [`StoreError::Closed`].
    pub(crate) async fn run<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move |txn: &mut Transaction<'_>| {
            let result = (|| {
                let sp = txn.savepoint().map_err(backend)?;
                let value = f(&sp)?;
                sp.commit().map_err(backend)?;
                Ok(value)
            })();
            Box::new(move |commit: Result<(), StoreError>| {
                let _ = tx.send(commit.and(result));
            })
        });
        let jobs = self.jobs.as_ref().ok_or(StoreError::Closed)?;
        jobs.send(job).await.map_err(|_| StoreError::Closed)?;
        rx.await.map_err(|_| StoreError::Closed)?
    }
}

fn run_batch(conn: &mut Connection, batch: Vec<Job>) {
    let mut txn = match conn.transaction() {
        Ok(txn) => txn,
        Err(e) => {
            // Dropping the jobs drops their reply channels: each caller gets
            // StoreError::Closed and nothing was written.
            tracing::error!(error = %backend(e), jobs = batch.len(), "cannot start a write transaction");
            return;
        }
    };
    let finishes: Vec<Finish> = batch.into_iter().map(|job| job(&mut txn)).collect();
    let commit = txn.commit().map_err(backend);
    if let Err(e) = &commit {
        tracing::error!(error = %e, "write transaction failed to commit");
    }
    for finish in finishes {
        finish(commit.clone());
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // Closing the channel lets the thread finish the queue and exit.
        self.jobs.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
