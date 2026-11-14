use super::*;

/// At most one plan is being persisted or awaiting acknowledgement. A plan
/// enters the codec queue only after its authenticated receipt and directories
/// have been synced. Joining this owned thread also covers cancellation during
/// an uninterruptible filesystem sync.
pub(super) struct PlanWriter {
    sender: Option<tokio::sync::mpsc::Sender<Plan>>,
    pub receiver: tokio::sync::mpsc::Receiver<Result<(Plan, u64)>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl PlanWriter {
    pub fn new(
        directory: &Path,
        master: &MasterKey,
        operation: Uuid,
        stop: Arc<AtomicBool>,
        counters: Arc<Counters>,
    ) -> Self {
        let directory = directory.join("plans");
        let master = master.clone();
        let (sender, mut requests) = tokio::sync::mpsc::channel::<Plan>(1);
        let (results, receiver) = tokio::sync::mpsc::channel(1);
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while let Some(plan) = requests.blocking_recv() {
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let started = Instant::now();
                    #[cfg(test)]
                    if let Some(hooks) = &counters.hooks {
                        hooks.before_plan(plan.index, &stopped)?;
                    }
                    write_batch(
                        &directory.join(format!("{}.enc", plan.index)),
                        &format!("backup-stream:{operation}:plan:{}", plan.index),
                        &master,
                        &plan,
                    )?;
                    #[cfg(test)]
                    if let Some(hooks) = &counters.hooks {
                        hooks.after_plan(plan.index, &stopped)?;
                    }
                    Ok((plan, elapsed_ms(started)))
                }))
                .unwrap_or_else(|_| Err(StateError::Storage("plan writer panicked".into())));
                let failed = result.is_err();
                if results.blocking_send(result).is_err() || failed {
                    break;
                }
            }
            // Keep the test-only hooks scoped to the same writer lifetime.
            drop(counters);
        });
        Self {
            sender: Some(sender),
            receiver,
            stop,
            thread: Some(thread),
        }
    }

    pub fn submit(&self, plan: Plan) -> Result<()> {
        self.sender
            .as_ref()
            .expect("live plan writer")
            .try_send(plan)
            .map_err(|_| StateError::Storage("streaming plan writer unavailable".into()))
    }
}
impl Drop for PlanWriter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.sender.take();
        self.receiver.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
