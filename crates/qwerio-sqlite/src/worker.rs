//! A dedicated thread that owns one SQLite connection.
//!
//! All work on the connection runs on this thread. Callers submit jobs through
//! a bounded queue. Running statements are cancelled through a per-job flag
//! checked by SQLite's progress handler (plus the interrupt handle); queued
//! requests are discarded before they start.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use qwerio_core::driver::CancelOutcome;
use qwerio_core::error::{DatabaseError, DbResult};
use qwerio_core::ids::RequestId;
use rusqlite::{Connection, InterruptHandle};
use tokio::sync::oneshot;

/// Jobs waiting behind the running one. Further submissions fail as busy.
const JOB_QUEUE_CAPACITY: usize = 32;

/// Virtual machine instructions between cancellation checks.
const PROGRESS_INTERVAL_OPS: i32 = 1_000;

/// How a job is started: normally, or discarded because it was cancelled
/// while queued. Discarded jobs must not touch the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobStart {
    Run,
    Discarded,
}

type JobFn = Box<dyn FnOnce(&mut Connection, JobStart) + Send>;

struct Job {
    request: Option<RequestId>,
    run: JobFn,
}

/// `sqlite3_interrupt` is a no-op when no statement is stepping, so a cancel
/// that arrives between preparing and stepping would be lost. The progress
/// handler checks this flag instead; it is reset when each job starts.
#[derive(Default)]
struct WorkerState {
    running: Option<RequestId>,
    queued: HashSet<RequestId>,
    discarded: HashSet<RequestId>,
    cancel_requested: Arc<AtomicBool>,
}

pub(crate) struct Worker {
    sender: Mutex<Option<SyncSender<Job>>>,
    state: Arc<Mutex<WorkerState>>,
    interrupt: InterruptHandle,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Worker {
    /// Starts the thread and opens the connection on it. `open` returns the
    /// connection and any initialization data the caller needs.
    pub(crate) async fn spawn<T, F>(thread_name: String, open: F) -> DbResult<(Self, T)>
    where
        T: Send + 'static,
        F: FnOnce() -> DbResult<(Connection, T)> + Send + 'static,
    {
        let (ready_sender, ready_receiver) = oneshot::channel();
        let (job_sender, job_receiver) = mpsc::sync_channel::<Job>(JOB_QUEUE_CAPACITY);
        let state = Arc::new(Mutex::new(WorkerState::default()));
        let thread_state = Arc::clone(&state);

        thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let (mut connection, init) = match open() {
                    Ok(opened) => opened,
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                        return;
                    }
                };

                let interrupt = connection.get_interrupt_handle();
                let cancel_requested = Arc::clone(&lock(&thread_state).cancel_requested);
                let installed = connection.progress_handler(
                    PROGRESS_INTERVAL_OPS,
                    Some(move || cancel_requested.load(Ordering::SeqCst)),
                );

                if let Err(error) = installed {
                    let message = format!("Could not install the SQLite progress handler: {error}");
                    let _ = ready_sender.send(Err(DatabaseError::internal(message)));
                    return;
                }

                if ready_sender.send(Ok((interrupt, init))).is_err() {
                    return;
                }

                for job in job_receiver {
                    let start = begin_job(&thread_state, job.request);
                    (job.run)(&mut connection, start);
                    lock(&thread_state).running = None;
                }
            })
            .map_err(|error| {
                DatabaseError::internal(format!("Could not start the SQLite worker: {error}"))
            })?;

        let (interrupt, init) = ready_receiver
            .await
            .map_err(|_| DatabaseError::internal("The SQLite worker stopped while opening"))??;

        let worker = Self {
            sender: Mutex::new(Some(job_sender)),
            state,
            interrupt,
        };

        Ok((worker, init))
    }

    /// Queues a job. Fails when the queue is full or the worker has stopped.
    pub(crate) fn submit(
        &self,
        request: Option<RequestId>,
        run: impl FnOnce(&mut Connection, JobStart) + Send + 'static,
    ) -> DbResult<()> {
        let sender = lock(&self.sender)
            .clone()
            .ok_or_else(DatabaseError::session_closed)?;

        if let Some(request) = request {
            lock(&self.state).queued.insert(request);
        }

        let job = Job {
            request,
            run: Box::new(run),
        };

        sender.try_send(job).map_err(|error| {
            if let Some(request) = request {
                lock(&self.state).queued.remove(&request);
            }

            match error {
                TrySendError::Full(_) => DatabaseError::session_busy(),
                TrySendError::Disconnected(_) => DatabaseError::session_closed(),
            }
        })
    }

    /// Runs a closure on the connection and returns its result.
    pub(crate) async fn call<T, F>(&self, work: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> DbResult<T> + Send + 'static,
    {
        let (result_sender, result_receiver) = oneshot::channel();

        self.submit(None, move |connection, _start| {
            let _ = result_sender.send(work(connection));
        })?;

        result_receiver
            .await
            .map_err(|_| DatabaseError::session_closed())?
    }

    pub(crate) fn cancel(&self, request: RequestId) -> CancelOutcome {
        let mut state = lock(&self.state);

        if state.running == Some(request) {
            state.cancel_requested.store(true, Ordering::SeqCst);
            self.interrupt.interrupt();
            return CancelOutcome::Requested;
        }

        if state.queued.contains(&request) {
            state.discarded.insert(request);
            return CancelOutcome::Discarded;
        }

        CancelOutcome::NotRunning
    }

    /// Stops accepting jobs and interrupts the running one. Queued jobs still
    /// run (or are discarded) so their callers receive a result.
    pub(crate) fn shut_down(&self) {
        lock(&self.sender).take();

        let state = lock(&self.state);

        if state.running.is_some() {
            state.cancel_requested.store(true, Ordering::SeqCst);
            self.interrupt.interrupt();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shut_down();
    }
}

fn begin_job(state: &Mutex<WorkerState>, request: Option<RequestId>) -> JobStart {
    let mut state = lock(state);
    state.cancel_requested.store(false, Ordering::SeqCst);

    let Some(request) = request else {
        return JobStart::Run;
    };

    state.queued.remove(&request);

    if state.discarded.remove(&request) {
        return JobStart::Discarded;
    }

    state.running = Some(request);
    JobStart::Run
}
