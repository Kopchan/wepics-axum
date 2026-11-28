//! Shared FFmpeg preview transcodes.
//!
//! A job is identified by its cache key (`<direction><size>/<hash>`). The first
//! request starts FFmpeg and every request for the same key attaches to the
//! same in-memory buffer while the encoder runs. The job lives in its own Tokio
//! task, so closing a viewer connection neither stops FFmpeg nor throws the
//! result away: the clip is still written to the cache for later requests.
//!
//! At most [`MAX_CONCURRENT_TRANSCODES`] FFmpeg processes run at a time. Jobs
//! beyond that wait for a permit instead of spawning more encoders, which keeps
//! high-resolution videos from exhausting CPU and RAM.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

use axum::body::Bytes;
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tokio::sync::{Semaphore, watch};
use tokio_stream::Stream;

/// How many FFmpeg encoders may run at the same time.
///
/// One job already decodes and encodes a high-resolution video with several
/// threads, so a larger number starves the machine of CPU and RAM.
pub const MAX_CONCURRENT_TRANSCODES: usize = 2;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Deduplicates concurrent transcodes and caps how many run in parallel.
#[derive(Clone)]
pub struct TranscodeHub {
    in_flight: Arc<Mutex<HashMap<String, Arc<ActiveTranscode>>>>,
    semaphore: Arc<Semaphore>,
    running: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

struct ActiveTranscode {
    state: Mutex<TranscodeState>,
    /// Publishes [`TranscodeState::version`] so waiting streams wake up.
    notify: watch::Sender<u64>,
}

struct TranscodeState {
    /// Every byte produced so far. Late subscribers replay it from the start.
    buffer: Vec<u8>,
    status: TranscodeStatus,
    /// Incremented under the state lock on every buffer or status change.
    version: u64,
}

#[derive(Clone)]
enum TranscodeStatus {
    Running,
    Finished,
    Failed(String),
}

impl ActiveTranscode {
    fn new() -> Self {
        Self {
            state: Mutex::new(TranscodeState {
                buffer: Vec::new(),
                status: TranscodeStatus::Running,
                version: 0,
            }),
            notify: watch::channel(0).0,
        }
    }

    fn lock(&self) -> MutexGuard<'_, TranscodeState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn publish(&self, version: u64) {
        // `send_replace` also updates the value when nobody is subscribed yet;
        // `send` would drop the update and leave late subscribers waiting.
        self.notify.send_replace(version);
    }

    fn append(&self, bytes: &[u8]) {
        let version = {
            let mut state = self.lock();
            state.buffer.extend_from_slice(bytes);
            state.version += 1;
            state.version
        };
        self.publish(version);
    }

    /// Copies an already cached clip to every subscriber of a queued job.
    fn finish_with(&self, buffer: Vec<u8>) {
        let version = {
            let mut state = self.lock();
            state.buffer = buffer;
            state.status = TranscodeStatus::Finished;
            state.version += 1;
            state.version
        };
        self.publish(version);
    }

    fn finish(&self) {
        let version = {
            let mut state = self.lock();
            state.status = TranscodeStatus::Finished;
            state.version += 1;
            state.version
        };
        self.publish(version);
    }

    fn fail(&self, message: String) {
        let version = {
            let mut state = self.lock();
            state.status = TranscodeStatus::Failed(message);
            state.version += 1;
            state.version
        };
        self.publish(version);
    }
}

/// Removes the in-flight entry when the job task ends, whatever the outcome.
struct InFlightGuard {
    in_flight: Arc<Mutex<HashMap<String, Arc<ActiveTranscode>>>>,
    key: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut map = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.remove(&self.key);
    }
}

/// Counts a job as running between getting a permit and its task ending.
struct RunningGuard {
    running: Arc<AtomicUsize>,
}

impl RunningGuard {
    fn enter(running: Arc<AtomicUsize>, peak: Arc<AtomicUsize>) -> Self {
        let active = running.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(active, Ordering::SeqCst);
        Self { running }
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.running.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Default for TranscodeHub {
    fn default() -> Self {
        Self {
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_TRANSCODES)),
            running: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl TranscodeHub {
    /// Number of FFmpeg jobs running right now.
    pub fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    /// Highest number of simultaneous FFmpeg jobs observed by this hub.
    pub fn peak_concurrency(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    /// Attaches to the job for `key`, starting it in the background if needed.
    ///
    /// The returned stream replays every byte produced for the job, including
    /// bytes written before the caller subscribed. `spawn_fn` is only called
    /// once a concurrency permit is available, so queued requests do not start
    /// extra encoder processes.
    pub fn subscribe<F>(
        &self,
        key: String,
        cache_dir: PathBuf,
        final_path: PathBuf,
        spawn_fn: F,
    ) -> HubTranscodeStream
    where
        F: FnOnce() -> Result<Child, String> + Send + 'static,
    {
        let job = {
            let mut map = self
                .in_flight
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());

            if let Some(existing) = map.get(&key) {
                Arc::clone(existing)
            } else {
                let job = Arc::new(ActiveTranscode::new());
                map.insert(key.clone(), Arc::clone(&job));

                let guard = InFlightGuard {
                    in_flight: Arc::clone(&self.in_flight),
                    key,
                };
                let job_background = Arc::clone(&job);
                let semaphore = Arc::clone(&self.semaphore);
                let running = Arc::clone(&self.running);
                let peak = Arc::clone(&self.peak);

                tokio::spawn(async move {
                    // The guard lives in the task: a dropped HTTP response only
                    // drops its stream, never the encoder.
                    let _guard = guard;
                    run_transcode_job(
                        job_background,
                        semaphore,
                        running,
                        peak,
                        cache_dir,
                        final_path,
                        spawn_fn,
                    )
                    .await;
                });

                job
            }
        };

        HubTranscodeStream {
            job,
            offset: 0,
            wait_fut: None,
        }
    }
}

async fn run_transcode_job<F>(
    job: Arc<ActiveTranscode>,
    semaphore: Arc<Semaphore>,
    running: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    cache_dir: PathBuf,
    final_path: PathBuf,
    spawn_fn: F,
) where
    F: FnOnce() -> Result<Child, String>,
{
    let _permit = match semaphore.acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return job.fail("transcode queue is closed".into()),
    };
    let _running = RunningGuard::enter(running, peak);

    // While this job was queued another request may have produced the clip.
    if let Ok(existing) = fs::read(&final_path).await
        && !existing.is_empty()
    {
        return job.finish_with(existing);
    }

    if let Err(err) = fs::create_dir_all(&cache_dir).await {
        return job.fail(format!("cannot create transcode cache dir: {err}"));
    }

    let tmp_path = cache_dir.join(format!(
        ".transc.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut tmp_file = match File::create(&tmp_path).await {
        Ok(file) => file,
        Err(err) => return job.fail(format!("cannot create temp file: {err}")),
    };

    let mut child = match spawn_fn() {
        Ok(child) => child,
        Err(err) => {
            drop(tmp_file);
            let _ = fs::remove_file(&tmp_path).await;
            return job.fail(err);
        }
    };

    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill().await;
            drop(tmp_file);
            let _ = fs::remove_file(&tmp_path).await;
            return job.fail("ffmpeg produced no output pipe".into());
        }
    };

    if let Some(mut stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            let text = text.trim();
            if !text.is_empty() {
                eprintln!("transcode: {text}");
            }
        });
    }

    let mut chunk = vec![0_u8; 64 * 1024];
    loop {
        let read = match stdout.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) => {
                let _ = child.kill().await;
                drop(tmp_file);
                let _ = fs::remove_file(&tmp_path).await;
                return job.fail(format!("cannot read ffmpeg output: {err}"));
            }
        };

        // Publish before writing to disk so viewers see frames while FFmpeg is
        // still producing them; the cache write must not delay the stream.
        job.append(&chunk[..read]);

        if let Err(err) = tmp_file.write_all(&chunk[..read]).await {
            let _ = child.kill().await;
            drop(tmp_file);
            let _ = fs::remove_file(&tmp_path).await;
            return job.fail(format!("cannot write transcode cache: {err}"));
        }
    }

    let status = child.wait().await;
    if !matches!(&status, Ok(status) if status.success()) {
        drop(tmp_file);
        let _ = fs::remove_file(&tmp_path).await;
        let reason = match status {
            Ok(status) => format!("ffmpeg exited with {status}"),
            Err(err) => format!("cannot wait for ffmpeg: {err}"),
        };
        return job.fail(reason);
    }

    if let Err(err) = tmp_file.flush().await {
        drop(tmp_file);
        let _ = fs::remove_file(&tmp_path).await;
        return job.fail(format!("cannot flush transcode cache: {err}"));
    }
    drop(tmp_file);

    // An atomic rename means readers never observe a half-written clip.
    if let Err(err) = fs::rename(&tmp_path, &final_path).await {
        // Every byte is already in the buffer, so a cache failure must not turn
        // a playable clip into an error for current viewers.
        eprintln!("transcode: cannot cache {}: {err}", final_path.display());
        let _ = fs::remove_file(&tmp_path).await;
    }

    job.finish();
}

/// Byte stream for one HTTP client, replaying a shared job from `offset`.
pub struct HubTranscodeStream {
    job: Arc<ActiveTranscode>,
    offset: usize,
    wait_fut:
        Option<Pin<Box<dyn Future<Output = Result<(), watch::error::RecvError>> + Send>>>,
}

enum Action {
    /// New bytes and the buffer length after them.
    Data(Bytes, usize),
    Wait,
    Done,
    Failed(String),
}

impl Stream for HubTranscodeStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let (action, version) = {
                let state = self.job.lock();
                let action = if self.offset < state.buffer.len() {
                    Action::Data(
                        Bytes::copy_from_slice(&state.buffer[self.offset..]),
                        state.buffer.len(),
                    )
                } else {
                    match &state.status {
                        TranscodeStatus::Running => Action::Wait,
                        TranscodeStatus::Finished => Action::Done,
                        TranscodeStatus::Failed(message) => Action::Failed(message.clone()),
                    }
                };
                (action, state.version)
            };

            match action {
                Action::Data(bytes, length) => {
                    self.offset = length;
                    self.wait_fut = None;
                    return Poll::Ready(Some(Ok(bytes)));
                }
                Action::Done => return Poll::Ready(None),
                Action::Failed(message) => {
                    return Poll::Ready(Some(Err(std::io::Error::other(message))));
                }
                Action::Wait => {
                    if self.wait_fut.is_none() {
                        let mut rx = self.job.notify.subscribe();
                        if *rx.borrow_and_update() != version {
                            // A chunk or a status change landed between the
                            // snapshot and subscribing: read again instead of
                            // waiting for a notification that already happened.
                            continue;
                        }
                        self.wait_fut = Some(Box::pin(async move {
                            rx.changed().await.map(|_| ())
                        }));
                    }

                    match self.wait_fut.as_mut().unwrap().as_mut().poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok(())) => self.wait_fut = None,
                        // The sender cannot disappear while this stream holds an
                        // `Arc` to the job; treat it as the end of the clip.
                        Poll::Ready(Err(_)) => return Poll::Ready(None),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio_stream::StreamExt;

    use super::*;

    #[tokio::test]
    async fn stream_replays_bytes_written_before_subscribing() {
        let job = Arc::new(ActiveTranscode::new());
        job.append(b"first");
        let mut stream = HubTranscodeStream {
            job: Arc::clone(&job),
            offset: 0,
            wait_fut: None,
        };

        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&*chunk, b"first");

        job.append(b"second");
        job.finish();

        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&*chunk, b"second");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn stream_wakes_up_when_a_waiting_job_produces_data() {
        let job = Arc::new(ActiveTranscode::new());
        let mut stream = HubTranscodeStream {
            job: Arc::clone(&job),
            offset: 0,
            wait_fut: None,
        };

        let producer = tokio::spawn({
            let job = Arc::clone(&job);
            async move {
                tokio::task::yield_now().await;
                job.append(b"late");
                job.finish();
            }
        });

        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&*chunk, b"late");
        assert!(stream.next().await.is_none());
        producer.await.unwrap();
    }

    #[tokio::test]
    async fn finished_empty_job_ends_the_stream() {
        let job = Arc::new(ActiveTranscode::new());
        job.finish();
        let mut stream = HubTranscodeStream {
            job,
            offset: 0,
            wait_fut: None,
        };

        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn failed_job_surfaces_its_message() {
        let job = Arc::new(ActiveTranscode::new());
        job.fail("ffmpeg exploded".into());
        let mut stream = HubTranscodeStream {
            job,
            offset: 0,
            wait_fut: None,
        };

        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.to_string(), "ffmpeg exploded");
    }
}
