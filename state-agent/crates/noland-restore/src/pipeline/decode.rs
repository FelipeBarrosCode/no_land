//! Bounded, restore-owned decode workers. CPU-heavy authentication/decompression
//! must not serialize every pack on the task polling the download window.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

use noland_crypto::MasterKey;
use noland_pack::PackStreamDecoder;
use noland_state_core::{Result, StateError};
use noland_storage::DownloadStreamConsumer;
use tokio::sync::{mpsc, oneshot, watch};

use super::{elapsed_us, AssemblyMessage, PackJob, PipelineIoMetrics};

pub(super) const INPUT_BYTES: usize = 256 * 1024;

#[derive(Default)]
pub(super) struct DecodeStats {
    decode_us: AtomicU64,
    dispatch_wait_us: AtomicU64,
    active: AtomicU64,
    peak: AtomicU64,
}

impl DecodeStats {
    pub fn snapshot(&self, io: &mut PipelineIoMetrics) {
        io.pack_decode_us = self.decode_us.load(Ordering::Relaxed);
        io.decode_dispatch_wait_us = self.dispatch_wait_us.load(Ordering::Relaxed);
        io.peak_decode_workers = self.peak.load(Ordering::Relaxed);
    }
}

struct ActiveWorker(Arc<DecodeStats>);
impl Drop for ActiveWorker {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

type Reply = oneshot::Sender<Result<()>>;
enum Work {
    Restart,
    Write(Vec<u8>),
    Finish,
    #[cfg(test)]
    Test(Box<dyn FnOnce() + Send>),
}

pub(super) struct StreamConsumer {
    input: Option<mpsc::Sender<(Instant, Work, Reply)>>,
    stopped: watch::Sender<bool>,
    worker: Option<std::thread::JoinHandle<()>>,
    received: Arc<AtomicU64>,
}

impl StreamConsumer {
    pub fn new(
        job: PackJob,
        master: &MasterKey,
        output: mpsc::Sender<AssemblyMessage>,
        queue_wait_us: Arc<AtomicU64>,
        stats: Arc<DecodeStats>,
    ) -> Result<Self> {
        // One input and one response per stream: a download cannot run ahead of
        // its decoder or accumulate unbounded plaintext outside the assembly queue.
        let (input, mut receiver) = mpsc::channel::<(Instant, Work, Reply)>(1);
        let (stopped, mut stop) = watch::channel(false);
        let received = Arc::new(AtomicU64::new(0));
        let worker_received = Arc::clone(&received);
        let master = master.clone();
        let worker = std::thread::Builder::new().name("restore-decode".into()).spawn(move || {
            let active = stats.active.fetch_add(1, Ordering::Relaxed) + 1;
            stats.peak.fetch_max(active, Ordering::Relaxed);
            let _active = ActiveWorker(Arc::clone(&stats));
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else { return; };
            runtime.block_on(async move {
                let mut decoder = None;
                loop {
                    let request = tokio::select! {
                        biased;
                        _ = stop.changed() => break,
                        request = receiver.recv() => request,
                    };
                    let Some((queued, work, reply)) = request else { break; };
                    stats.dispatch_wait_us.fetch_add(elapsed_us(queued), Ordering::Relaxed);
                    let finish = matches!(work, Work::Finish);
                    let result = async {
                        match work {
                            Work::Restart => {
                                decoder = Some(PackStreamDecoder::new(&job.id, job.entries.clone())?);
                                worker_received.store(0, Ordering::Relaxed);
                            }
                            Work::Write(bytes) => {
                                let decoder = decoder.as_mut().ok_or_else(|| StateError::Storage("restore decoder was not initialized".into()))?;
                                let mut bytes = bytes.as_slice();
                                while !bytes.is_empty() {
                                    if *stop.borrow() { return Err(cancelled()); }
                                    let count = bytes.len().min(decoder.next_record_input_limit());
                                    let mut record = None;
                                    let started = Instant::now();
                                    let decoded = decoder.push_owned(&bytes[..count], &master, |entry, plain| {
                                        record = Some((entry.chunk_hash.clone(), plain));
                                        Ok(())
                                    });
                                    stats.decode_us.fetch_add(elapsed_us(started), Ordering::Relaxed);
                                    decoded?;
                                    worker_received.store(decoder.received_bytes(), Ordering::Relaxed);
                                    bytes = &bytes[count..];
                                    if let Some((hash, plain)) = record {
                                        let queued = Instant::now();
                                        let sent = tokio::select! {
                                            biased;
                                            _ = stop.changed() => return Err(cancelled()),
                                            sent = output.send(AssemblyMessage::Chunk(hash, plain)) => sent,
                                        };
                                        queue_wait_us.fetch_add(elapsed_us(queued), Ordering::Relaxed);
                                        sent.map_err(|_| StateError::Storage("restore assembly stopped".into()))?;
                                    }
                                }
                            }
                            Work::Finish => {
                                let decoder = decoder.as_ref().ok_or_else(|| StateError::Storage("restore decoder was not initialized".into()))?;
                                decoder.finish()?;
                                if decoder.received_bytes() < job.size {
                                    return Err(StateError::Integrity("streamed pack size mismatch".into()));
                                }
                                tokio::select! {
                                    biased;
                                    _ = stop.changed() => return Err(cancelled()),
                                    sent = output.send(AssemblyMessage::Flush) => {
                                        sent.map_err(|_| StateError::Storage("restore assembly stopped".into()))?;
                                    }
                                }
                            }
                            #[cfg(test)]
                            Work::Test(work) => work(),
                        }
                        Ok(())
                    }.await;
                    let failed = result.is_err();
                    let _ = reply.send(result);
                    if failed || finish { break; }
                }
            });
        })?;
        Ok(Self {
            input: Some(input),
            stopped,
            worker: Some(worker),
            received,
        })
    }

    async fn request(&self, work: Work) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.input
            .as_ref()
            .expect("live decode worker")
            .send((Instant::now(), work, reply))
            .await
            .map_err(|_| StateError::Storage("restore decoder stopped".into()))?;
        result
            .await
            .map_err(|_| StateError::Storage("restore decoder stopped".into()))?
    }

    pub fn received_bytes(&self) -> u64 {
        self.received.load(Ordering::Relaxed)
    }
}

fn cancelled() -> StateError {
    StateError::Storage("restore decoding cancelled".into())
}

#[async_trait::async_trait]
impl DownloadStreamConsumer for StreamConsumer {
    async fn restart(&self) -> Result<()> {
        self.request(Work::Restart).await
    }
    async fn write(&self, bytes: &[u8]) -> Result<()> {
        for bytes in bytes.chunks(INPUT_BYTES) {
            self.request(Work::Write(bytes.to_vec())).await?;
        }
        Ok(())
    }
    async fn finish(&self) -> Result<()> {
        self.request(Work::Finish).await
    }
}

impl Drop for StreamConsumer {
    fn drop(&mut self) {
        // Cancellation can interrupt a worker blocked on a full assembly queue.
        // Join before the outer restore guard can roll back destination partials.
        self.stopped.send_replace(true);
        self.input.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    fn job() -> PackJob {
        PackJob {
            id: uuid::Uuid::new_v4().to_string(),
            entries: Vec::new(),
            size: 0,
            priority: super::super::RestorePriority::Critical,
            first_use: (0, 0),
            selected_chunk_references: 0,
            selected_plaintext_bytes: 0,
        }
    }

    #[tokio::test]
    async fn decode_workers_run_concurrently_without_blocking_the_single_thread_executor() {
        let key = MasterKey::generate();
        let stats = Arc::new(DecodeStats::default());
        let (output, _receiver) = mpsc::channel(1);
        let first = StreamConsumer::new(
            job(),
            &key,
            output.clone(),
            Arc::default(),
            Arc::clone(&stats),
        )
        .unwrap();
        let second =
            StreamConsumer::new(job(), &key, output, Arc::default(), Arc::clone(&stats)).unwrap();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (started_a, running_a) = oneshot::channel();
        let (started_b, running_b) = oneshot::channel();
        let work = |started: oneshot::Sender<std::thread::ThreadId>| {
            let release = Arc::clone(&release);
            Work::Test(Box::new(move || {
                let _ = started.send(std::thread::current().id());
                let (lock, notify) = &*release;
                let (released, _) = notify
                    .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(2), |ready| {
                        !*ready
                    })
                    .unwrap();
                assert!(*released, "decoder blocked its caller or another decoder");
            }))
        };
        let observe = async {
            let (a, b) = tokio::join!(running_a, running_b);
            assert_ne!(a.as_ref().unwrap(), b.as_ref().unwrap());
            assert_ne!(*a.as_ref().unwrap(), std::thread::current().id());
            *release.0.lock().unwrap() = true;
            release.1.notify_all();
        };
        let (a, b, ()) = tokio::join!(
            first.request(work(started_a)),
            second.request(work(started_b)),
            observe
        );
        a.unwrap();
        b.unwrap();
        assert_eq!(stats.peak.load(Ordering::Relaxed), 2);
        drop(first);
        drop(second);
        assert_eq!(stats.active.load(Ordering::Relaxed), 0);
    }

    fn encrypted_fixture() -> (std::path::PathBuf, MasterKey, PackJob, Vec<u8>) {
        let root =
            std::env::temp_dir().join(format!("noland-decode-test-{}", uuid::Uuid::new_v4()));
        let key = MasterKey::generate();
        let payload = b"authenticated fixture".to_vec();
        let pack = noland_pack::pack_chunks(
            &root,
            &key,
            vec![(noland_cas::blake3_hex(&payload), payload)],
            |_| false,
        )
        .unwrap()
        .remove(0);
        let bytes = std::fs::read(&pack.path).unwrap();
        let job = PackJob {
            id: pack.pack_id,
            entries: pack.entries,
            size: bytes.len() as u64,
            priority: super::super::RestorePriority::Critical,
            first_use: (0, 0),
            selected_chunk_references: 1,
            selected_plaintext_bytes: 21,
        };
        (root, key, job, bytes)
    }

    #[tokio::test]
    async fn cancellation_joins_a_decoder_blocked_on_a_full_assembly_queue() {
        let (root, key, job, bytes) = encrypted_fixture();
        let stats = Arc::new(DecodeStats::default());
        let (output, mut receiver) = mpsc::channel(1);
        output.send(AssemblyMessage::Flush).await.unwrap();
        let consumer =
            StreamConsumer::new(job, &key, output, Arc::default(), Arc::clone(&stats)).unwrap();
        consumer.restart().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), consumer.write(&bytes))
                .await
                .is_err()
        );
        let started = Instant::now();
        drop(consumer);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(stats.active.load(Ordering::Relaxed), 0);
        assert!(matches!(
            receiver.recv().await,
            Some(AssemblyMessage::Flush)
        ));
        assert!(receiver.recv().await.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn restart_resets_only_the_cipher_stream_and_authentication_is_still_mandatory() {
        let (root, key, job, bytes) = encrypted_fixture();
        let (output, mut receiver) = mpsc::channel(8);
        let consumer =
            StreamConsumer::new(job, &key, output, Arc::default(), Arc::default()).unwrap();
        consumer.restart().await.unwrap();
        consumer.write(&bytes).await.unwrap();
        consumer.restart().await.unwrap();
        assert_eq!(consumer.received_bytes(), 0);
        let mut tampered = bytes.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(consumer.write(&tampered).await.is_err());
        drop(consumer);
        assert!(matches!(
            receiver.recv().await,
            Some(AssemblyMessage::Chunk(_, _))
        ));
        assert!(receiver.recv().await.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
