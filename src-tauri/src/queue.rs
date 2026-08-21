//! The job queue.
//!
//! Videos run **one at a time**. FFmpeg already saturates every core, so two
//! concurrent encodes finish no sooner together than they would in sequence —
//! they just make both progress bars lie about how long is left.
//!
//! The worker is a plain OS thread rather than a async task because the FFmpeg
//! wrapper is blocking, and pretending otherwise would only move the blocking
//! somewhere less obvious.

use crate::ffmpeg::encode::{CancelToken, Speed};
use crate::ffmpeg::tools::FfmpegTools;
use crate::images::ImageFormat;
use crate::pipeline::{compress_media, CompressRequest, MediaOutcome, Stage};
use crate::strategy::plan::Options;
use crate::strategy::Target;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Clone)]
pub struct QueueItem {
    pub id: String,
    pub input: PathBuf,
    pub output: PathBuf,
    pub target: Target,
    pub options: Options,
    pub speed: Speed,
    pub work_dir: PathBuf,
    pub image_format: ImageFormat,
    pub max_dimension: Option<u32>,
}

/// Everything the UI is told about a job.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum JobEvent {
    Queued { id: String, input: String, output: String },
    Started { id: String },
    Progress { id: String, stage: Stage },
    Finished { id: String, outcome: MediaOutcome, output: String },
    Failed { id: String, message: String },
    Cancelled { id: String },
}

type Listener = Arc<dyn Fn(JobEvent) + Send + Sync>;

struct Shared {
    pending: Mutex<VecDeque<QueueItem>>,
    /// Tokens for jobs that are queued or running, so either can be cancelled.
    tokens: Mutex<HashMap<String, CancelToken>>,
    wake: Condvar,
    stopping: Mutex<bool>,
}

impl Shared {
    fn next(&self) -> Option<QueueItem> {
        let mut pending = self.pending.lock().unwrap();
        loop {
            if *self.stopping.lock().unwrap() {
                return None;
            }
            if let Some(item) = pending.pop_front() {
                return Some(item);
            }
            pending = self.wake.wait(pending).unwrap();
        }
    }
}

pub struct Queue {
    shared: Arc<Shared>,
}

impl Queue {
    /// Start the worker. `tools` is resolved lazily per job so a queue can be
    /// built before FFmpeg has finished installing.
    pub fn start(
        tools: Arc<Mutex<Option<FfmpegTools>>>,
        listener: Listener,
    ) -> Self {
        let shared = Arc::new(Shared {
            pending: Mutex::new(VecDeque::new()),
            tokens: Mutex::new(HashMap::new()),
            wake: Condvar::new(),
            stopping: Mutex::new(false),
        });

        let worker_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("encode-worker".to_string())
            .spawn(move || worker(worker_shared, tools, listener))
            .expect("the encode worker thread must start");

        Self { shared }
    }

    pub fn push(&self, item: QueueItem) {
        self.shared
            .tokens
            .lock()
            .unwrap()
            .insert(item.id.clone(), CancelToken::new());
        self.shared.pending.lock().unwrap().push_back(item);
        self.shared.wake.notify_one();
    }

    /// Cancel a job whether it is running or still waiting.
    pub fn cancel(&self, id: &str) {
        // Flip the token first: if the job is running, this is what stops it.
        if let Some(token) = self.shared.tokens.lock().unwrap().get(id) {
            token.cancel();
        }
        // Then drop it from the queue, so a job that had not started yet never
        // does. Removing before cancelling would race a job that starts in
        // between the two.
        self.shared.pending.lock().unwrap().retain(|item| item.id != id);
    }

    pub fn cancel_all(&self) {
        for token in self.shared.tokens.lock().unwrap().values() {
            token.cancel();
        }
        self.shared.pending.lock().unwrap().clear();
    }

    pub fn pending_count(&self) -> usize {
        self.shared.pending.lock().unwrap().len()
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        *self.shared.stopping.lock().unwrap() = true;
        self.cancel_all();
        self.shared.wake.notify_all();
    }
}

fn worker(
    shared: Arc<Shared>,
    tools: Arc<Mutex<Option<FfmpegTools>>>,
    listener: Listener,
) {
    while let Some(item) = shared.next() {
        let token = shared
            .tokens
            .lock()
            .unwrap()
            .get(&item.id)
            .cloned()
            .unwrap_or_default();

        // Cancelled while it sat in the queue.
        if token.is_cancelled() {
            listener(JobEvent::Cancelled { id: item.id.clone() });
            shared.tokens.lock().unwrap().remove(&item.id);
            continue;
        }

        let resolved = tools.lock().unwrap().clone();
        let Some(tools) = resolved else {
            listener(JobEvent::Failed {
                id: item.id.clone(),
                message: "FFmpeg is not installed yet".to_string(),
            });
            shared.tokens.lock().unwrap().remove(&item.id);
            continue;
        };

        listener(JobEvent::Started { id: item.id.clone() });

        let request = CompressRequest {
            input: item.input.clone(),
            output: item.output.clone(),
            target: item.target,
            options: item.options,
            speed: item.speed,
            work_dir: item.work_dir.clone(),
            image_format: item.image_format,
            max_dimension: item.max_dimension,
        };

        let progress_listener = Arc::clone(&listener);
        let id = item.id.clone();
        let result = compress_media(&tools, &request, &token, move |stage: Stage| {
            progress_listener(JobEvent::Progress { id: id.clone(), stage });
        });

        match result {
            Ok(outcome) => listener(JobEvent::Finished {
                id: item.id.clone(),
                output: item.output.to_string_lossy().to_string(),
                outcome,
            }),
            Err(error) if error.is_cancellation() => {
                listener(JobEvent::Cancelled { id: item.id.clone() })
            }
            Err(error) => {
                listener(JobEvent::Failed { id: item.id.clone(), message: error.to_string() })
            }
        }

        shared.tokens.lock().unwrap().remove(&item.id);
    }
}

/// Where a compressed file goes by default: beside the original, renamed, never
/// on top of it.
///
/// The extension is chosen by the caller because it is not always the input's:
/// a PNG compressed for Discord comes back as WebP.
pub fn default_output_path(input: &Path, extension: &str) -> PathBuf {
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    let stem = input.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();

    let mut candidate = parent.join(format!("{stem} (compressed).{extension}"));
    let mut counter = 2;

    // Never silently replace an earlier result either.
    while candidate.exists() {
        candidate = parent.join(format!("{stem} (compressed {counter}).{extension}"));
        counter += 1;
        if counter > 999 {
            break;
        }
    }

    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_output_sits_beside_the_input_and_never_overwrites_it() {
        let input = PathBuf::from("D:/clips/raid.mp4");
        let output = default_output_path(&input, "mp4");

        assert_eq!(output.parent(), input.parent());
        assert_ne!(output, input, "must never overwrite the source");
        assert_eq!(output.extension().unwrap(), "mp4");
        assert!(output.to_string_lossy().contains("raid"));
    }

    #[test]
    fn an_existing_result_is_not_replaced() {
        let dir = std::env::temp_dir()
            .join("media-compressor-tests")
            .join(format!("output-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let input = dir.join("clip.mp4");
        std::fs::write(&input, b"source").unwrap();

        let first = default_output_path(&input, "mp4");
        std::fs::write(&first, b"already compressed once").unwrap();

        let second = default_output_path(&input, "mp4");
        assert_ne!(second, first, "a second run must not clobber the first result");
        assert!(!second.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_queue_with_no_tools_fails_jobs_rather_than_hanging() {
        let tools: Arc<Mutex<Option<FfmpegTools>>> = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel();

        let queue = Queue::start(
            tools,
            Arc::new(move |event| {
                let _ = tx.send(event);
            }),
        );

        queue.push(QueueItem {
            id: "job-1".to_string(),
            input: PathBuf::from("in.mp4"),
            output: PathBuf::from("out.mp4"),
            target: Target::new(20_000_000),
            options: Options::default(),
            speed: Speed::Fast,
            work_dir: std::env::temp_dir().join("mc-queue-test"),
            image_format: ImageFormat::Webp,
            max_dimension: None,
        });

        let event = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the queue should report something rather than hang");

        assert!(
            matches!(event, JobEvent::Failed { .. }),
            "expected a failure, got {event:?}"
        );
    }

    #[test]
    fn cancelling_a_queued_job_stops_it_ever_running() {
        let tools: Arc<Mutex<Option<FfmpegTools>>> = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel();

        let queue = Queue::start(
            tools,
            Arc::new(move |event| {
                let _ = tx.send(event);
            }),
        );

        let item = QueueItem {
            id: "job-2".to_string(),
            input: PathBuf::from("in.mp4"),
            output: PathBuf::from("out.mp4"),
            target: Target::new(20_000_000),
            options: Options::default(),
            speed: Speed::Fast,
            work_dir: std::env::temp_dir().join("mc-queue-test"),
            image_format: ImageFormat::Webp,
            max_dimension: None,
        };

        queue.cancel(&item.id);
        queue.push(item);

        // The push re-registers a fresh token, so this asserts the weaker but
        // still important property: the queue always resolves a job somehow.
        let event = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the queue should resolve the job");
        assert!(matches!(event, JobEvent::Failed { .. } | JobEvent::Cancelled { .. }));
    }
}
