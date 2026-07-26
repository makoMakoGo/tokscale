use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use crate::subscription::{ProviderId, SubscriptionBatch};

use super::generation_controller::{
    load_background_data, persist_background_load, run_acquisition_task, AcquisitionTaskResult,
    BackgroundLoad,
};

/// Owns every task whose lifetime is bounded by the interactive TUI session.
///
/// A task handle must not be detached: acquisition can write the generation
/// cache, while subscription fetches can retain network work. Shutdown first
/// signals cancellation, then aborts async work and joins every owned task.
pub(super) struct TaskSupervisor {
    runtime: tokio::runtime::Handle,
    acquisition_tx: mpsc::Sender<AcquisitionTaskResult>,
    acquisition_rx: mpsc::Receiver<AcquisitionTaskResult>,
    cancelled: Arc<AtomicBool>,
    persistence_gate: Arc<Mutex<()>>,
    acquisition_tasks: Vec<thread::JoinHandle<()>>,
    subscription_tasks: Vec<tokio::task::JoinHandle<()>>,
    drained: bool,
}

impl TaskSupervisor {
    pub(super) fn new(runtime: tokio::runtime::Handle) -> Self {
        let (acquisition_tx, acquisition_rx) = mpsc::channel();
        Self {
            runtime,
            acquisition_tx,
            acquisition_rx,
            cancelled: Arc::new(AtomicBool::new(false)),
            persistence_gate: Arc::new(Mutex::new(())),
            acquisition_tasks: Vec::new(),
            subscription_tasks: Vec::new(),
            drained: false,
        }
    }

    pub(super) fn spawn_acquisition(
        &mut self,
        request_id: u64,
        engine: tokenx_engine::AcquisitionEngine,
        force: bool,
        last_digest: Option<u64>,
    ) {
        self.reap_finished();
        let tx = self.acquisition_tx.clone();
        let runtime = self.runtime.clone();
        let cancelled = Arc::clone(&self.cancelled);
        let persistence_gate = Arc::clone(&self.persistence_gate);

        self.acquisition_tasks.push(thread::spawn(move || {
            run_acquisition_task(&tx, request_id, || {
                let loaded = runtime.block_on(load_background_data(&engine, force, last_digest));
                let _persistence_guard = persistence_gate
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if cancelled.load(Ordering::Acquire) {
                    return Ok(BackgroundLoad::Unchanged);
                }
                persist_background_load(loaded)
            });
        }));
    }

    pub(super) fn spawn_subscription_fetch(
        &mut self,
        enabled: Vec<ProviderId>,
        tx: mpsc::Sender<SubscriptionBatch>,
    ) {
        self.reap_finished();
        self.subscription_tasks.push(self.runtime.spawn(async move {
            let batch = crate::subscription::service::fetch_enabled(&enabled).await;
            let _ = tx.send(batch);
        }));
    }

    pub(super) fn try_recv_acquisition(
        &self,
    ) -> std::result::Result<AcquisitionTaskResult, mpsc::TryRecvError> {
        self.acquisition_rx.try_recv()
    }

    /// Prevents any new cache persistence and aborts outstanding async work.
    pub(super) fn cancel(&mut self) {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        for task in &self.subscription_tasks {
            task.abort();
        }

        // Synchronize with a persistence operation that passed its cancellation
        // check immediately before shutdown. Once this lock is observed, every
        // later acquisition sees cancellation before it can write the cache.
        drop(
            self.persistence_gate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
    }

    /// Drains all cancelled work. Call after terminal restoration so a slow
    /// blocking acquisition cannot strand the user in raw terminal mode.
    pub(super) fn drain(&mut self) {
        if self.drained {
            return;
        }
        self.cancel();
        self.drained = true;
        let subscription_tasks = std::mem::take(&mut self.subscription_tasks);
        self.runtime.block_on(async move {
            for task in subscription_tasks {
                let _ = task.await;
            }
        });

        for task in self.acquisition_tasks.drain(..) {
            let _ = task.join();
        }
    }

    fn reap_finished(&mut self) {
        let mut index = 0;
        while index < self.acquisition_tasks.len() {
            if self.acquisition_tasks[index].is_finished() {
                let task = self.acquisition_tasks.swap_remove(index);
                let _ = task.join();
            } else {
                index += 1;
            }
        }
        self.subscription_tasks.retain(|task| !task.is_finished());
    }
}

impl Drop for TaskSupervisor {
    fn drop(&mut self) {
        self.drain();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    use super::TaskSupervisor;

    struct Dropped(Arc<AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn shutdown_aborts_and_drains_subscription_tasks() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let mut supervisor = TaskSupervisor::new(runtime.handle().clone());
        let dropped = Arc::new(AtomicBool::new(false));
        let task_dropped = Arc::clone(&dropped);
        let (started_tx, started_rx) = mpsc::channel();

        supervisor
            .subscription_tasks
            .push(runtime.spawn(async move {
                let _guard = Dropped(task_dropped);
                started_tx.send(()).expect("test receiver remains open");
                tokio::time::sleep(Duration::from_secs(60)).await;
            }));
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("task started");

        supervisor.cancel();
        supervisor.drain();

        assert!(dropped.load(Ordering::Acquire));
        assert!(supervisor.subscription_tasks.is_empty());
    }

    #[test]
    fn shutdown_cancels_and_joins_acquisition_threads() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let mut supervisor = TaskSupervisor::new(runtime.handle().clone());
        let cancelled = Arc::clone(&supervisor.cancelled);
        let joined = Arc::new(AtomicBool::new(false));
        let task_joined = Arc::clone(&joined);

        supervisor
            .acquisition_tasks
            .push(std::thread::spawn(move || {
                while !cancelled.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                task_joined.store(true, Ordering::Release);
            }));

        supervisor.cancel();
        supervisor.drain();

        assert!(joined.load(Ordering::Acquire));
        assert!(supervisor.acquisition_tasks.is_empty());
    }
}
