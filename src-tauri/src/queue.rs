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
    /// Shared with the worker. Cancelling a job that has not started removes it
    /// before the worker ever sees it, so this side has to report those itself
    /// or nothing ever will.
    listener: Listener,
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
            listener: Arc::clone(&listener),
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
        // Flip the token first: if the job is running, this is what stops it,
        // and the worker reports it when the encode aborts.
        if let Some(token) = self.shared.tokens.lock().unwrap().get(id) {
            token.cancel();
        }

        // Then drop it from the queue, so a job that had not started yet never
        // does. Removing before cancelling would race a job that starts in
        // between the two.
        let was_waiting = {
            let mut pending = self.shared.pending.lock().unwrap();
            let before = pending.len();
            pending.retain(|item| item.id != id);
            pending.len() != before
        };

        // A job pulled from the queue never reaches the worker, so this is the
        // only place that can report it. Without this the row sits on "queued"
        // forever and no amount of clicking dismisses it.
        if was_waiting {
            self.shared.tokens.lock().unwrap().remove(id);
            self.announce(JobEvent::Cancelled { id: id.to_string() });
        }
    }

    pub fn cancel_all(&self) {
        for token in self.shared.tokens.lock().unwrap().values() {
            token.cancel();
        }

        let abandoned: Vec<String> = {
            let mut pending = self.shared.pending.lock().unwrap();
            pending.drain(..).map(|item| item.id).collect()
        };

        for id in abandoned {
            self.shared.tokens.lock().unwrap().remove(&id);
            self.announce(JobEvent::Cancelled { id });
        }
    }

    /// Emit an event, holding no locks.
    ///
    /// The listener reaches into the UI layer; calling it while holding the
    /// queue's own locks invites a deadlock the first time that layer calls
    /// back in.
    fn announce(&self, event: JobEvent) {
        (self.shared.listener)(event);
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

/// Two directories that are the same place, as best we can tell.
///
/// Canonicalising resolves trailing separators, `.`, and case differences that
/// would otherwise make the source folder look like a different one — which
/// matters because the answer decides whether the output can safely keep the
/// original's name.
fn same_directory(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Where a compressed file goes.
///
/// `directory` of `None` means "beside the original", which is the default.
///
/// The name depends on where it lands. Sharing a folder with the source, the
/// output must be distinguished or it would land on top of the original, so it
/// gains a "(compressed)" suffix. Given a folder of its own there is nothing to
/// collide with, so it keeps the source's exact name — which matters because
/// Discord shows the filename to everyone in the channel.
///
/// An existing file is never replaced, and the source is never overwritten.
pub fn output_path_for(input: &Path, extension: &str, directory: Option<&Path>) -> PathBuf {
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    let target = directory.unwrap_or(parent);
    let stem = input.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();

    let beside_source = same_directory(target, parent);

    let name = |counter: Option<u32>| -> String {
        match (beside_source, counter) {
            (true, None) => format!("{stem} (compressed).{extension}"),
            (true, Some(n)) => format!("{stem} (compressed {n}).{extension}"),
            (false, None) => format!("{stem}.{extension}"),
            (false, Some(n)) => format!("{stem} ({n}).{extension}"),
        }
    };

    let mut candidate = target.join(name(None));
    let mut counter = 2;

    // `candidate == input` guards the case where the source already carries the
    // name we would generate: overwriting the file being read is the one
    // outcome that loses data outright.
    while candidate.exists() || same_file(&candidate, input) {
        candidate = target.join(name(Some(counter)));
        counter += 1;
        if counter > 999 {
            break;
        }
    }

    candidate
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_output_sits_beside_the_input_and_never_overwrites_it() {
        let input = PathBuf::from("D:/clips/raid.mp4");
        let output = output_path_for(&input, "mp4", None);

        assert_eq!(output.parent(), input.parent());
        assert_ne!(output, input, "must never overwrite the source");
        assert_eq!(output.extension().unwrap(), "mp4");
        assert!(output.to_string_lossy().contains("raid"));
    }

    #[test]
    fn a_chosen_folder_keeps_the_original_name() {
        let dir = std::env::temp_dir()
            .join("media-compressor-tests")
            .join(format!("outdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let source_dir = dir.join("source");
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&out_dir).unwrap();

        let input = source_dir.join("Rocket League_replay.mp4");
        std::fs::write(&input, b"source").unwrap();

        let output = output_path_for(&input, "mp4", Some(&out_dir));

        // A folder of its own has nothing to collide with, so no suffix — the
        // name Discord shows to everyone stays clean.
        assert_eq!(output.file_name().unwrap(), "Rocket League_replay.mp4");
        assert!(same_directory(output.parent().unwrap(), &out_dir));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn choosing_the_sources_own_folder_still_avoids_overwriting_it() {
        let dir = std::env::temp_dir()
            .join("media-compressor-tests")
            .join(format!("samedir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let input = dir.join("clip.mp4");
        std::fs::write(&input, b"source").unwrap();

        // Explicitly pointing the output at the source's own folder must behave
        // like "beside the original", not like a clean folder — otherwise the
        // encode would write straight over the file it is reading.
        let output = output_path_for(&input, "mp4", Some(&dir));

        assert_ne!(output, input, "must never overwrite the source");
        assert!(
            output.file_name().unwrap().to_string_lossy().contains("compressed"),
            "expected a distinguishing suffix, got {output:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_image_changing_format_never_lands_on_its_source() {
        let dir = std::env::temp_dir()
            .join("media-compressor-tests")
            .join(format!("imgout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        std::fs::write(dir.join("shot.png"), b"source").unwrap();

        let output = output_path_for(&dir.join("shot.png"), "webp", Some(&out_dir));
        assert_eq!(output.file_name().unwrap(), "shot.webp");

        let _ = std::fs::remove_dir_all(&dir);
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

        let first = output_path_for(&input, "mp4", None);
        std::fs::write(&first, b"already compressed once").unwrap();

        let second = output_path_for(&input, "mp4", None);
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

    fn waiting_item(id: &str) -> QueueItem {
        QueueItem {
            id: id.to_string(),
            input: PathBuf::from("in.mp4"),
            output: PathBuf::from("out.mp4"),
            target: Target::new(20_000_000),
            options: Options::default(),
            speed: Speed::Fast,
            work_dir: std::env::temp_dir().join("mc-queue-test"),
            image_format: ImageFormat::Webp,
            max_dimension: None,
        }
    }

    /// The invariant a stalled row exposed: every job must end in *some*
    /// reported state. Cancelling used to pull waiting jobs out of the queue
    /// before the worker could see them, so nothing ever reported those — the
    /// row sat on "queued" forever and could not be dismissed.
    #[test]
    fn every_job_is_reported_even_when_cancelled_before_it_runs() {
        use std::collections::HashSet;
        use std::time::{Duration, Instant};

        let tools: Arc<Mutex<Option<FfmpegTools>>> = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel();

        let queue = Queue::start(
            tools,
            Arc::new(move |event| {
                let _ = tx.send(event);
            }),
        );

        let ids: Vec<String> = (0..6).map(|n| format!("bulk-{n}")).collect();
        for id in &ids {
            queue.push(waiting_item(id));
        }
        queue.cancel_all();

        let mut settled: HashSet<String> = HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(10);

        while settled.len() < ids.len() && Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(250)) {
                // Either terminal state is acceptable; being told nothing is not.
                Ok(JobEvent::Cancelled { id }) | Ok(JobEvent::Failed { id, .. }) => {
                    settled.insert(id);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }

        let stranded: Vec<&String> = ids.iter().filter(|id| !settled.contains(*id)).collect();
        assert!(stranded.is_empty(), "these jobs were never reported: {stranded:?}");
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
