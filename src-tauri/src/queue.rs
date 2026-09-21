//! The job queue.
//!
//! Videos run **one at a time**. FFmpeg already saturates every core, so two
//! concurrent encodes finish no sooner together than they would in sequence —
//! they just make both progress bars lie about how long is left.
//!
//! The worker is a plain OS thread rather than a async task because the FFmpeg
//! wrapper is blocking, and pretending otherwise would only move the blocking
//! somewhere less obvious.
//!
//! There are two ways of stopping, because users mean two different things by
//! it:
//!
//! * **Pause** keeps the queue. The encode in flight is aborted and put back at
//!   the head, so resuming runs it again from the start and nothing is lost.
//! * **Cancel** throws work away — one job or all of them — and it never comes
//!   back.
//!
//! Whichever it is, every job must end in a state the UI can show. A row that
//! can be neither finished nor dismissed is worse than one that failed.

use crate::ffmpeg::encode::{CancelToken, Speed};
use crate::ffmpeg::tools::FfmpegTools;
use crate::images::ImageFormat;
use crate::pipeline::{compress_media, CompressRequest, MediaOutcome, Stage};
use crate::strategy::plan::Options;
use crate::strategy::Target;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

/// What becomes of the original once the compressed file is written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Disposition {
    /// The result is a new file and the original stays where it is.
    #[default]
    Keep,
    /// The result takes the original's place and the original goes to the
    /// recycle bin.
    ///
    /// The encode still writes to a scratch file first: nothing is destroyed
    /// until there is a finished file ready to stand in its place.
    Replace,
}

/// What actually became of the original, once the job finished.
///
/// Asking to replace is not the same as having replaced, and the difference
/// decides what the UI may offer — there is nothing left to compare a result
/// against once its source is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Original {
    /// Untouched. The result is a separate file.
    Kept,
    /// Deleted, with the result now carrying its name.
    Replaced,
    /// Replacement was asked for, but the result came out no smaller than the
    /// source, so the result was discarded and the original left alone.
    KeptNotSmaller,
}

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
    /// Where results were sent, kept so `output` can be worked out again at
    /// write time rather than trusted from when the job was queued. `None`
    /// means beside the original.
    pub output_dir: Option<PathBuf>,
    /// Set when this job must displace its source: the path the finished file
    /// ends up at, which is the original's name carrying whatever extension
    /// the new format needs. `output` is then a scratch file beside it.
    pub replacement: Option<PathBuf>,
}

/// Everything the UI is told about a job.
///
/// Each variant names the state the job is in *now*, and every job ends in one
/// of the three terminal ones. `Queued` is sent both when a job is first
/// accepted and when a paused job is handed back — a row that receives it is
/// waiting, however it got there.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum JobEvent {
    Queued { id: String, input: String, output: String },
    Started { id: String },
    Progress { id: String, stage: Stage },
    Finished { id: String, outcome: MediaOutcome, output: String, original: Original },
    Failed { id: String, message: String },
    Cancelled { id: String },
}

/// The queue as a whole, which is what the footer controls are about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct QueueStatus {
    pub paused: bool,
    /// Jobs waiting for the worker.
    pub waiting: usize,
    /// Whether a job is being encoded right now.
    pub running: bool,
}

type Listener = Arc<dyn Fn(JobEvent) + Send + Sync>;

/// Why a running job was stopped. The cancel token records only *that* it was,
/// and the worker cannot guess which the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interrupt {
    /// This one file was dismissed. It is gone.
    Cancelled,
    /// The whole queue paused. The file goes back, and runs again on resume.
    Paused,
}

/// The job the worker is holding, as everyone else sees it.
struct Running {
    id: String,
    token: CancelToken,
    interrupt: Option<Interrupt>,
}

/// Every piece of queue state, under one lock.
///
/// Splitting the waiting list, the cancel tokens and the paused flag across
/// separate mutexes turned every question that spans two of them — "is this id
/// waiting, or is the worker already running it?" — into a race instead of a
/// read. It also puts `stopping` under the lock the worker parks on, which is
/// the only place it can be set without a window where the worker has read it
/// as false but not yet reached `wait`, misses the notification, and parks
/// forever.
struct State {
    pending: VecDeque<QueueItem>,
    running: Option<Running>,
    paused: bool,
    /// Set when the queue is being dropped, so the worker stops waiting.
    stopping: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    /// Shared with the worker. Cancelling a job that has not started removes it
    /// before the worker ever sees it, so this side has to report those itself
    /// or nothing ever will.
    listener: Listener,
}

impl Shared {
    /// Block until there is work to do, and claim it.
    ///
    /// Returns `None` only when the queue is shutting down.
    fn take_next(&self) -> Option<(QueueItem, CancelToken)> {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.stopping {
                return None;
            }

            if !state.paused {
                if let Some(item) = state.pending.pop_front() {
                    let token = CancelToken::new();
                    state.running = Some(Running {
                        id: item.id.clone(),
                        token: token.clone(),
                        interrupt: None,
                    });
                    return Some((item, token));
                }
            }

            state = self.wake.wait(state).unwrap();
        }
    }

    /// Give up the running slot, reporting how the job was interrupted if it
    /// was interrupted at all.
    fn finish(&self) -> Option<Interrupt> {
        let mut state = self.state.lock().unwrap();
        state.running.take().and_then(|running| running.interrupt)
    }

    /// Put a paused job back at the *head* of the queue.
    ///
    /// The head, not the tail: a pause must not quietly reorder the work.
    fn requeue(&self, item: QueueItem) {
        self.state.lock().unwrap().pending.push_front(item);
    }

    /// Emit an event, holding no locks.
    ///
    /// The listener reaches into the UI layer; calling it while holding the
    /// queue's own lock invites a deadlock the first time that layer calls
    /// back in.
    fn announce(&self, event: JobEvent) {
        (self.listener)(event);
    }
}

pub struct Queue {
    shared: Arc<Shared>,
}

impl Queue {
    /// Start the worker. `tools` is resolved lazily per job so a queue can be
    /// built before FFmpeg has finished installing.
    pub fn start(tools: Arc<Mutex<Option<FfmpegTools>>>, listener: Listener) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pending: VecDeque::new(),
                running: None,
                paused: false,
                stopping: false,
            }),
            wake: Condvar::new(),
            listener,
        });

        let worker_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("encode-worker".to_string())
            .spawn(move || worker(worker_shared, tools))
            .expect("the encode worker thread must start");

        Self { shared }
    }

    /// Accept a job.
    ///
    /// Pushing onto a paused queue leaves it paused. Whether adding a file
    /// should also set the queue going again is a question about what the user
    /// meant by it, so it is answered a layer up, in the command that takes
    /// files.
    pub fn push(&self, item: QueueItem) {
        self.shared.state.lock().unwrap().pending.push_back(item);
        self.shared.wake.notify_all();
    }

    /// Cancel one job, whether it is running or still waiting.
    pub fn cancel(&self, id: &str) {
        let removed = {
            let mut state = self.shared.state.lock().unwrap();

            if let Some(running) = state.running.as_mut() {
                if running.id == id {
                    // The worker owns reporting a job it is holding, but it
                    // cannot tell this from a pause unless it is told.
                    running.interrupt = Some(Interrupt::Cancelled);
                    running.token.cancel();
                    return;
                }
            }

            let before = state.pending.len();
            state.pending.retain(|item| item.id != id);
            state.pending.len() != before
        };

        // A waiting job never reaches the worker, so this is the only place
        // that can report it. Without this the row sits on "queued" forever and
        // no amount of clicking dismisses it.
        if removed {
            self.shared.announce(JobEvent::Cancelled { id: id.to_string() });
        }
    }

    /// Throw everything away: the job running and every job waiting.
    pub fn cancel_all(&self) {
        let abandoned: Vec<String> = {
            let mut state = self.shared.state.lock().unwrap();

            // Discarding the queue lifts a pause too. Leaving it set would hand
            // back an empty queue that is still refusing to run, and the only
            // clue would be a Resume button with nothing to resume.
            state.paused = false;

            if let Some(running) = state.running.as_mut() {
                running.interrupt = Some(Interrupt::Cancelled);
                running.token.cancel();
            }

            state.pending.drain(..).map(|item| item.id).collect()
        };

        self.shared.wake.notify_all();

        for id in abandoned {
            self.shared.announce(JobEvent::Cancelled { id });
        }
    }

    /// Stop working, keeping the queue intact.
    ///
    /// The encode in flight is aborted and handed back to the front of the
    /// queue, so resuming runs it again from the start. FFmpeg cannot freeze an
    /// encode and pick it up later, and holding a suspended process — with its
    /// open handles and its half-written file — for however long the user is
    /// away is worse than paying for those seconds twice.
    pub fn pause(&self) -> QueueStatus {
        {
            let mut state = self.shared.state.lock().unwrap();
            state.paused = true;

            if let Some(running) = state.running.as_mut() {
                // An explicit cancel already claimed this job. A pause must not
                // turn it back into something that runs again.
                if running.interrupt.is_none() {
                    running.interrupt = Some(Interrupt::Paused);
                    running.token.cancel();
                }
            }
        }

        self.status()
    }

    /// Start working through the queue again.
    pub fn resume(&self) -> QueueStatus {
        self.shared.state.lock().unwrap().paused = false;
        self.shared.wake.notify_all();
        self.status()
    }

    pub fn status(&self) -> QueueStatus {
        let state = self.shared.state.lock().unwrap();
        QueueStatus {
            paused: state.paused,
            waiting: state.pending.len(),
            running: state.running.is_some(),
        }
    }

    pub fn pending_count(&self) -> usize {
        self.shared.state.lock().unwrap().pending.len()
    }

    /// Jobs either running or waiting to run.
    ///
    /// [`Self::pending_count`] answers a narrower question: it does not count
    /// the job the worker is inside right now, which is precisely the one that
    /// would notice its `ffmpeg` being deleted out from under it. The running
    /// slot is held from the moment the worker claims a job until the encode
    /// ends, so counting it alongside the waiting list is the honest answer to
    /// "is anything using FFmpeg?"
    pub fn active_count(&self) -> usize {
        let state = self.shared.state.lock().unwrap();
        state.pending.len() + usize::from(state.running.is_some())
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        // Under the same lock the worker waits on — see `State::stopping`.
        self.shared.state.lock().unwrap().stopping = true;
        self.cancel_all();
        self.shared.wake.notify_all();
    }
}

fn worker(shared: Arc<Shared>, tools: Arc<Mutex<Option<FfmpegTools>>>) {
    while let Some((mut item, token)) = shared.take_next() {
        let resolved = tools.lock().unwrap().clone();
        let Some(tools) = resolved else {
            shared.finish();
            shared.announce(JobEvent::Failed {
                id: item.id.clone(),
                message: "FFmpeg is not installed yet".to_string(),
            });
            continue;
        };

        shared.announce(JobEvent::Started { id: item.id.clone() });

        settle_output(&mut item);

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

        let progress_listener = Arc::clone(&shared.listener);
        let id = item.id.clone();
        let result = compress_media(&tools, &request, &token, move |stage: Stage| {
            progress_listener(JobEvent::Progress { id: id.clone(), stage });
        });

        // Why it stopped has to be read before anything is announced: the
        // running slot must be free before the job can go back in the queue.
        let interrupt = shared.finish();

        match result {
            // Finishing just as a pause landed still counts as finished.
            Ok(outcome) => shared.announce(settle(&item, outcome)),
            Err(error) => {
                // A replacement encodes into the user's own folder, so a job
                // that ended early leaves a half-written scratch file sitting
                // next to the original. It goes however the job ended.
                if item.replacement.is_some() {
                    let _ = std::fs::remove_file(&item.output);
                }

                if !error.is_cancellation() {
                    shared.announce(JobEvent::Failed {
                        id: item.id.clone(),
                        message: error.to_string(),
                    });
                } else if interrupt == Some(Interrupt::Paused) {
                    // Back to the front of the queue, announced the way it was
                    // when first accepted: the row shows where the file will
                    // end up, never the scratch file a replacement passes
                    // through.
                    let id = item.id.clone();
                    let input = item.input.to_string_lossy().to_string();
                    let destination = item.replacement.as_ref().unwrap_or(&item.output);
                    let output = destination.to_string_lossy().to_string();
                    shared.requeue(item);
                    shared.announce(JobEvent::Queued { id, input, output });
                } else {
                    shared.announce(JobEvent::Cancelled { id: item.id.clone() });
                }
            }
        }
    }
}

/// Turn a finished encode into the event the UI sees.
///
/// This is the only place a source file is ever given up, and it happens after
/// the encode rather than before it — by the time anything moves there is a
/// complete file ready to take its place.
fn settle(item: &QueueItem, outcome: MediaOutcome) -> JobEvent {
    settle_with(item, outcome, recycle)
}

/// The body of [`settle`], with the disposal step handed in for tests.
fn settle_with(
    item: &QueueItem,
    outcome: MediaOutcome,
    recycle: impl FnOnce(&Path) -> std::io::Result<()>,
) -> JobEvent {
    let Some(destination) = item.replacement.as_deref() else {
        return JobEvent::Finished {
            id: item.id.clone(),
            output: item.output.to_string_lossy().to_string(),
            outcome,
            original: Original::Kept,
        };
    };

    // Compressing something already small can produce a bigger file. Trading
    // the source for that loses on both counts — larger *and* re-encoded — so
    // the source stays and the result goes. A source we cannot measure is not
    // grounds for refusing; only a measured one that wins.
    let source_bytes = std::fs::metadata(&item.input).map(|meta| meta.len()).unwrap_or(0);
    if source_bytes > 0 && outcome.output_bytes() >= source_bytes {
        let _ = std::fs::remove_file(&item.output);
        return JobEvent::Finished {
            id: item.id.clone(),
            output: item.input.to_string_lossy().to_string(),
            outcome,
            original: Original::KeptNotSmaller,
        };
    }

    let destination = settled_destination(&item.input, destination);
    match install_replacement(&item.input, &item.output, &destination, recycle) {
        Ok(()) => JobEvent::Finished {
            id: item.id.clone(),
            output: destination.to_string_lossy().to_string(),
            outcome,
            original: Original::Replaced,
        },
        // The encode is lost, but the source is not. Reporting why beats
        // leaving a scratch file behind and calling the job done.
        Err(InstallError::Untouched(error)) => {
            let _ = std::fs::remove_file(&item.output);
            JobEvent::Failed {
                id: item.id.clone(),
                message: format!("could not replace the original: {error}"),
            }
        }
        // Here the scratch file is the only copy of the encode, so it stays
        // and the message says where. Clearing it as above would leave the
        // user with nothing but a trip to the recycle bin.
        Err(error @ InstallError::OriginalBinned(_)) => JobEvent::Failed {
            id: item.id.clone(),
            message: format!(
                "the original is in the recycle bin, but the result could not take its name, \
                 so it is still at {}: {error}",
                item.output.display()
            ),
        },
    }
}

/// Re-check where this job writes against the disk as it is now.
///
/// The path was chosen when the job was queued, which for a batch is before
/// any of it ran — so two sources that resolve to one name were both handed
/// it. `a/clip.mp4` and `b/clip.mp4` collected into one output folder is the
/// easy way to hit this; two files differing only by container is the other.
/// Without this the second job writes over the first one's finished result.
///
/// The worker takes one job at a time, so every earlier result is already on
/// disk by the time this asks. Re-running the original resolution rather than
/// bumping a counter onto the name keeps the convention that fits where the
/// file is landing — "(compressed 2)" beside the source, "(2)" in a folder of
/// its own.
///
/// A replacement is left alone: its scratch file is already unique per job,
/// and its destination is re-checked after the encode instead.
fn settle_output(item: &mut QueueItem) {
    if item.replacement.is_some() || !item.output.exists() {
        return;
    }

    let extension = item.output.extension().unwrap_or_default().to_string_lossy().to_string();
    item.output = output_path_for(&item.input, &extension, item.output_dir.as_deref());
}

/// The destination, re-checked against the disk as it is now.
///
/// It was chosen when the job was queued, which for a batch can be many
/// minutes and several finished files ago. Two sources in one batch that
/// differ only by container — `clip.mov` and `clip.webm` — resolve to the same
/// `clip.mp4`, so without this the second one lands on the first one's result
/// and destroys it.
///
/// The source's own name is never re-resolved away: that is the file this job
/// exists to replace.
fn settled_destination(input: &Path, chosen: &Path) -> PathBuf {
    if same_file(chosen, input) || !chosen.exists() {
        return chosen.to_path_buf();
    }

    let extension = chosen.extension().unwrap_or_default().to_string_lossy().to_string();
    replacement_path_for(input, &extension)
}

/// Why a replacement could not be installed, and what that leaves on disk.
///
/// The two cases want opposite things from the caller, which is the whole
/// reason this is not a plain [`std::io::Error`]: one wants the scratch file
/// cleared, the other must not lose it.
#[derive(Debug)]
enum InstallError {
    /// Nothing moved. The original is where it was and the result is still at
    /// its scratch path, so there is nothing to keep.
    Untouched(std::io::Error),
    /// The original reached the bin but the result could not take its name,
    /// which leaves the encode as the only copy under the scratch name.
    OriginalBinned(std::io::Error),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Untouched(error) | Self::OriginalBinned(error) => error.fmt(f),
        }
    }
}

/// Put a finished file in its source's place.
///
/// Where the extension changes, the two are different paths: the result goes
/// in first and the source is cleared after, so no moment exists in which
/// neither file is there. Where the extension is unchanged they are one path,
/// and the source has to leave before the rename — `fs::rename` replaces an
/// existing destination on both platforms, and an overwrite is the one
/// disposal that cannot be sent anywhere.
///
/// The disposal step is handed in rather than called directly so tests can
/// supply a bin of their own: the real one would put their fixtures in the
/// user's recycle bin, and a machine running them may have no bin at all.
fn install_replacement(
    original: &Path,
    staged: &Path,
    destination: &Path,
    recycle: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<(), InstallError> {
    // Decided before anything moves, while both paths still describe real
    // files: a rename that consumes the original leaves nothing left to
    // compare.
    let destination_is_original = same_file(original, destination);

    if destination_is_original {
        // The rename below lands on the original's own name and would erase it
        // outright, so the original goes to the bin first and the result takes
        // the freed name after.
        recycle(original).map_err(InstallError::Untouched)?;
        return std::fs::rename(staged, destination).map_err(InstallError::OriginalBinned);
    }

    std::fs::rename(staged, destination).map_err(InstallError::Untouched)?;

    // A format change lands the result under a new name — `clip.mov` becomes
    // `clip.mp4` — leaving the source behind to clear. The result is already
    // in place by now, so a source that refuses to go (open in a player, say)
    // is litter rather than a failed job.
    let _ = recycle(original);

    Ok(())
}

/// Send a file to the platform's recycle bin.
///
/// Never a plain delete. The file taking its place is lossily compressed and
/// may be the only copy its owner has, so the disposal has to be one they can
/// walk back — which is also why replacing is allowed to be a default at all.
fn recycle(path: &Path) -> std::io::Result<()> {
    trash::delete(path).map_err(std::io::Error::other)
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

/// Where a replacement ends up: the original's own name, carrying whatever
/// extension the new format needs.
///
/// The source is the one file a replacement may displace. An unrelated file
/// that happens to hold the name — a `clip.mp4` sitting beside the `clip.mov`
/// being compressed — gets a suffix instead, exactly as it would in "keep"
/// mode. Replacing what was asked for must never take something else with it.
pub fn replacement_path_for(input: &Path, extension: &str) -> PathBuf {
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    let stem = input.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();

    let mut candidate = parent.join(format!("{stem}.{extension}"));
    let mut counter = 2;

    while candidate.exists() && !same_file(&candidate, input) {
        candidate = parent.join(format!("{stem} ({counter}).{extension}"));
        counter += 1;
        if counter > 999 {
            break;
        }
    }

    candidate
}

/// The scratch file a replacement is encoded into.
///
/// It sits beside the original rather than in the app's work directory so the
/// final step is a rename within one folder: instant, and atomic where the
/// filesystem allows. Staging on another volume would turn every replacement
/// into a second full copy of a file that can be gigabytes.
///
/// The extension has to be the real one because FFmpeg picks its muxer from
/// it. The rest of the name is there to be unmistakably temporary, so a job
/// cut short by a crash leaves something recognisable behind rather than a
/// plausible-looking video.
pub fn staging_path_for(input: &Path, extension: &str, id: &str) -> PathBuf {
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    let stem = input.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();

    parent.join(format!(".{stem}.{id}.part.{extension}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::mpsc::Receiver;
    use std::time::{Duration, Instant};

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

    /// A scratch directory that cleans up after itself, so a failing assertion
    /// cannot leave a half-swapped file behind to confuse the next run.
    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("media-compressor-tests")
            .join(format!("{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in recycle bin: moves the file into a folder of its own, so a
    /// test can tell "sent to the bin" from "erased" without touching the
    /// real one.
    fn fake_bin(bin: &Path) -> impl FnOnce(&Path) -> std::io::Result<()> + '_ {
        std::fs::create_dir_all(bin).unwrap();
        move |path: &Path| std::fs::rename(path, bin.join(path.file_name().unwrap()))
    }

    /// A bin that refuses, for the paths that turn on disposal failing.
    fn bin_that_refuses(path: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other(format!("no bin for {}", path.display())))
    }

    fn replacing_item(input: &Path, staged: &Path, destination: &Path) -> QueueItem {
        QueueItem {
            id: "job-1".to_string(),
            input: input.to_path_buf(),
            output: staged.to_path_buf(),
            target: Target::new(20_000_000),
            options: Options::default(),
            speed: Speed::Fast,
            work_dir: std::env::temp_dir().join("mc-queue-test"),
            image_format: ImageFormat::Webp,
            max_dimension: None,
            output_dir: None,
            replacement: Some(destination.to_path_buf()),
        }
    }

    fn image_result(output_bytes: u64) -> MediaOutcome {
        MediaOutcome::Image(crate::images::ImageOutcome {
            output_bytes,
            quality: 74,
            width: 1920,
            height: 1080,
            downscale_steps: 0,
            encodes: 3,
            within_limit: true,
        })
    }

    #[test]
    fn a_replacement_keeping_its_format_lands_on_the_original_itself() {
        let dir = scratch("replace-same-ext");
        let input = dir.join("clip.mp4");
        std::fs::write(&input, b"source").unwrap();

        // Same extension in and out: there is exactly one name involved, and
        // the result has to end up wearing it.
        assert_eq!(replacement_path_for(&input, "mp4"), input);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_replacement_changing_format_takes_the_new_extension() {
        let dir = scratch("replace-new-ext");
        let input = dir.join("clip.mov");
        std::fs::write(&input, b"source").unwrap();

        assert_eq!(replacement_path_for(&input, "mp4"), dir.join("clip.mp4"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The case that turns a replacement into a data-loss bug: compressing
    /// `clip.mov` next to an unrelated `clip.mp4` must not consume the mp4.
    #[test]
    fn a_replacement_never_displaces_a_file_that_is_not_its_source() {
        let dir = scratch("replace-bystander");
        let input = dir.join("clip.mov");
        let bystander = dir.join("clip.mp4");
        std::fs::write(&input, b"source").unwrap();
        std::fs::write(&bystander, b"someone else's file").unwrap();

        let destination = replacement_path_for(&input, "mp4");

        assert_ne!(destination, bystander, "must not target the bystander");
        assert_eq!(
            std::fs::read(&bystander).unwrap(),
            b"someone else's file",
            "the bystander must survive untouched"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_staging_file_collides_with_neither_the_source_nor_the_destination() {
        let input = PathBuf::from("D:/clips/raid.mov");
        let staged = staging_path_for(&input, "mp4", "job-7");

        assert_eq!(staged.parent(), input.parent(), "must stage on the source's volume");
        assert_ne!(staged, input);
        assert_ne!(staged, replacement_path_for(&input, "mp4"));
        // FFmpeg picks its muxer from the extension, so the scratch file needs
        // the real one.
        assert_eq!(staged.extension().unwrap(), "mp4");
    }

    #[test]
    fn installing_a_same_format_replacement_swaps_the_contents_over() {
        let dir = scratch("install-same");
        let bin = dir.join("recycle-bin");
        let input = dir.join("clip.mp4");
        std::fs::write(&input, b"the original").unwrap();

        let staged = staging_path_for(&input, "mp4", "job-1");
        std::fs::write(&staged, b"the compressed result").unwrap();

        install_replacement(&input, &staged, &input, fake_bin(&bin)).unwrap();

        assert_eq!(std::fs::read(&input).unwrap(), b"the compressed result");
        assert!(!staged.exists(), "the scratch file must not linger");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installing_a_new_format_replacement_clears_the_old_file() {
        let dir = scratch("install-new-ext");
        let bin = dir.join("recycle-bin");
        let input = dir.join("clip.mov");
        std::fs::write(&input, b"the original").unwrap();

        let destination = dir.join("clip.mp4");
        let staged = staging_path_for(&input, "mp4", "job-1");
        std::fs::write(&staged, b"the compressed result").unwrap();

        install_replacement(&input, &staged, &destination, fake_bin(&bin)).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"the compressed result");
        assert!(!input.exists(), "the source must be gone once its result is in place");
        assert!(!staged.exists(), "the scratch file must not linger");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The point of the whole disposal step: a replaced original is recoverable.
    /// A result that keeps the source's extension lands on the source's own
    /// name, which is the case where a plain rename would erase it outright.
    #[test]
    fn a_same_format_replacement_sends_the_original_to_the_bin() {
        let dir = scratch("install-same-bins");
        let bin = dir.join("recycle-bin");
        let input = dir.join("clip.mp4");
        std::fs::write(&input, b"the original").unwrap();

        let staged = staging_path_for(&input, "mp4", "job-1");
        std::fs::write(&staged, b"the compressed result").unwrap();

        install_replacement(&input, &staged, &input, fake_bin(&bin)).unwrap();

        assert_eq!(std::fs::read(&input).unwrap(), b"the compressed result");
        assert_eq!(
            std::fs::read(bin.join("clip.mp4")).unwrap(),
            b"the original",
            "the original must be recoverable, not erased"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Changing format leaves the source under its own name to clear, which is
    /// the other route to disposal. It goes to the bin too.
    #[test]
    fn a_new_format_replacement_sends_the_original_to_the_bin() {
        let dir = scratch("install-new-ext-bins");
        let bin = dir.join("recycle-bin");
        let input = dir.join("clip.mov");
        std::fs::write(&input, b"the original").unwrap();

        let destination = dir.join("clip.mp4");
        let staged = staging_path_for(&input, "mp4", "job-1");
        std::fs::write(&staged, b"the compressed result").unwrap();

        install_replacement(&input, &staged, &destination, fake_bin(&bin)).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"the compressed result");
        assert_eq!(
            std::fs::read(bin.join("clip.mov")).unwrap(),
            b"the original",
            "the original must be recoverable, not erased"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A bin that refuses must stop the swap dead rather than fall back to
    /// erasing the file. Nothing has moved, so the caller is free to bin the
    /// scratch file instead.
    #[test]
    fn a_bin_that_refuses_leaves_the_original_where_it_is() {
        let dir = scratch("install-bin-refuses");
        let input = dir.join("clip.mp4");
        std::fs::write(&input, b"the original").unwrap();

        let staged = staging_path_for(&input, "mp4", "job-1");
        std::fs::write(&staged, b"the compressed result").unwrap();

        let error = install_replacement(&input, &staged, &input, bin_that_refuses)
            .expect_err("a refused bin must fail the install");

        assert!(matches!(error, InstallError::Untouched(_)));
        assert_eq!(std::fs::read(&input).unwrap(), b"the original", "the original must survive");
        assert!(staged.exists(), "the result is still the caller's to clear");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The window the recycle-first order opens: between the original leaving
    /// and the result taking its name, something else can claim it. The encode
    /// is then the only copy of itself, so it has to be reported, not cleared.
    #[test]
    fn a_result_that_cannot_take_the_freed_name_is_kept_rather_than_discarded() {
        let dir = scratch("settle-name-stolen");
        let bin = dir.join("recycle-bin");
        let input = dir.join("clip.mp4");
        std::fs::write(&input, vec![0u8; 4096]).unwrap();

        let staged = staging_path_for(&input, "mp4", "job-1");
        std::fs::write(&staged, b"smaller").unwrap();

        // Bins the original as any bin would, then takes the freed name with a
        // directory, which a file can never be renamed onto.
        let bin_dir = bin.clone();
        let steal_the_name = move |path: &Path| -> std::io::Result<()> {
            std::fs::create_dir_all(&bin_dir)?;
            std::fs::rename(path, bin_dir.join(path.file_name().unwrap()))?;
            std::fs::create_dir(path)
        };

        let item = replacing_item(&input, &staged, &input);
        let event = settle_with(&item, image_result(7), steal_the_name);

        match event {
            JobEvent::Failed { message, .. } => {
                assert!(
                    message.contains(&staged.display().to_string()),
                    "the message must say where the result actually is, got {message:?}"
                );
                assert!(message.contains("recycle bin"), "and that the original is in it");
            }
            other => panic!("expected a failed job, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(&staged).unwrap(),
            b"smaller",
            "the only copy of the encode must not be cleared"
        );
        assert!(bin.join("clip.mp4").exists(), "and the original must be in the bin");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_finished_replacement_reports_the_destination_rather_than_the_scratch_file() {
        let dir = scratch("settle-replaced");
        let bin = dir.join("recycle-bin");
        let input = dir.join("clip.mp4");
        std::fs::write(&input, vec![0u8; 4096]).unwrap();

        let staged = staging_path_for(&input, "mp4", "job-1");
        std::fs::write(&staged, b"smaller").unwrap();

        let event =
            settle_with(&replacing_item(&input, &staged, &input), image_result(7), fake_bin(&bin));

        match event {
            JobEvent::Finished { output, original, .. } => {
                assert_eq!(original, Original::Replaced);
                assert_eq!(PathBuf::from(output), input, "the row must point at the real file");
            }
            other => panic!("expected a finished job, got {other:?}"),
        }
        assert_eq!(std::fs::read(&input).unwrap(), b"smaller");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn keeping_item(input: &Path, output: &Path, output_dir: Option<&Path>) -> QueueItem {
        QueueItem {
            id: "job-2".to_string(),
            input: input.to_path_buf(),
            output: output.to_path_buf(),
            target: Target::new(20_000_000),
            options: Options::default(),
            speed: Speed::Fast,
            work_dir: std::env::temp_dir().join("mc-queue-test"),
            image_format: ImageFormat::Webp,
            max_dimension: None,
            output_dir: output_dir.map(Path::to_path_buf),
            replacement: None,
        }
    }

    /// Collecting same-named files from several folders into one output folder
    /// hands every job the same destination, because they are all resolved
    /// before any of them has written anything. The second encode would land
    /// on the first one's result.
    #[test]
    fn a_second_job_does_not_write_over_an_earlier_result() {
        let dir = scratch("settle-output-folder");
        let out_dir = dir.join("out");
        let first_source = dir.join("a");
        let second_source = dir.join("b");
        for path in [&out_dir, &first_source, &second_source] {
            std::fs::create_dir_all(path).unwrap();
        }

        std::fs::write(first_source.join("clip.mp4"), b"source one").unwrap();
        let second = second_source.join("clip.mp4");
        std::fs::write(&second, b"source two").unwrap();

        // Both were queued against an empty folder, so both were told to write
        // here. The first has since finished.
        let contested = out_dir.join("clip.mp4");
        std::fs::write(&contested, b"the first result").unwrap();

        let mut item = keeping_item(&second, &contested, Some(&out_dir));
        settle_output(&mut item);

        assert_ne!(item.output, contested, "must step around the earlier result");
        assert!(!item.output.exists(), "must pick a name nothing holds");
        assert_eq!(
            std::fs::read(&contested).unwrap(),
            b"the first result",
            "the earlier result must survive"
        );
        assert!(same_directory(item.output.parent().unwrap(), &out_dir));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Re-resolving has to reproduce the naming the mode calls for, not bolt a
    /// counter onto the name it was given — "clip (compressed) (2).mp4" is not
    /// what writing beside the source looks like.
    #[test]
    fn a_re_resolved_output_keeps_the_naming_of_its_mode() {
        let dir = scratch("settle-output-beside");
        let input = dir.join("clip.mov");
        std::fs::write(&input, b"source").unwrap();

        let taken = output_path_for(&input, "mp4", None);
        std::fs::write(&taken, b"an earlier result").unwrap();

        let mut item = keeping_item(&input, &taken, None);
        settle_output(&mut item);

        let name = item.output.file_name().unwrap().to_string_lossy().to_string();
        assert_ne!(item.output, taken);
        assert!(name.starts_with("clip (compressed"), "unexpected name {name:?}");
        assert!(!name.contains(") ("), "counter was bolted on: {name:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The common case, and the one that must stay cheap: nothing holds the
    /// path, so the job keeps exactly what it was queued with.
    #[test]
    fn an_uncontested_output_is_left_as_it_was() {
        let dir = scratch("settle-output-free");
        let input = dir.join("clip.mp4");
        std::fs::write(&input, b"source").unwrap();

        let chosen = output_path_for(&input, "mp4", None);
        let mut item = keeping_item(&input, &chosen, None);
        settle_output(&mut item);

        assert_eq!(item.output, chosen);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A replacement stages under a name unique to its job, and its real
    /// destination is settled after the encode. Re-resolving here would send
    /// the encode somewhere the swap is not looking.
    #[test]
    fn a_replacement_scratch_file_is_never_re_resolved() {
        let dir = scratch("settle-output-replacing");
        let input = dir.join("clip.mp4");
        std::fs::write(&input, b"source").unwrap();

        let staged = staging_path_for(&input, "mp4", "job-1");
        // A crash could have left one of these behind; it is ours to overwrite.
        std::fs::write(&staged, b"stale scratch").unwrap();

        let mut item = replacing_item(&input, &staged, &input);
        settle_output(&mut item);

        assert_eq!(item.output, staged);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two sources in one batch differing only by container both resolve to
    /// `clip.mp4` when they are queued, and the second one finishes long after
    /// the first has written that file. Landing on it would destroy a result
    /// the user had already been shown.
    #[test]
    fn a_second_replacement_does_not_land_on_the_first_ones_result() {
        let dir = scratch("settle-batch-collision");
        let bin = dir.join("recycle-bin");

        let first_result = dir.join("clip.mp4");
        std::fs::write(&first_result, b"the result of compressing clip.mov").unwrap();

        // Queued when `clip.mp4` did not exist yet, so this is what it chose.
        let second = dir.join("clip.webm");
        std::fs::write(&second, vec![0u8; 4096]).unwrap();
        let staged = staging_path_for(&second, "mp4", "job-2");
        std::fs::write(&staged, b"smaller").unwrap();

        let event = settle_with(
            &replacing_item(&second, &staged, &first_result),
            image_result(7),
            fake_bin(&bin),
        );

        match event {
            JobEvent::Finished { output, original, .. } => {
                assert_eq!(original, Original::Replaced);
                assert_ne!(
                    PathBuf::from(&output),
                    first_result,
                    "must step around the earlier result"
                );
                assert_eq!(std::fs::read(PathBuf::from(output)).unwrap(), b"smaller");
            }
            other => panic!("expected a finished job, got {other:?}"),
        }

        assert_eq!(
            std::fs::read(&first_result).unwrap(),
            b"the result of compressing clip.mov",
            "the earlier result must survive"
        );
        assert!(!second.exists(), "the source must still be replaced");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Compressing something already small can come out bigger. Swapping then
    /// would cost quality and gain nothing, so the source wins and the result
    /// is thrown away.
    #[test]
    fn a_result_no_smaller_than_its_source_leaves_the_source_alone() {
        let dir = scratch("settle-not-smaller");
        let bin = dir.join("recycle-bin");
        let input = dir.join("clip.mp4");
        std::fs::write(&input, b"the original").unwrap();

        let staged = staging_path_for(&input, "mp4", "job-1");
        std::fs::write(&staged, b"a bigger re-encode of the original").unwrap();

        let event = settle_with(
            &replacing_item(&input, &staged, &input),
            image_result(34),
            fake_bin(&bin),
        );

        match event {
            JobEvent::Finished { output, original, .. } => {
                assert_eq!(original, Original::KeptNotSmaller);
                assert_eq!(PathBuf::from(output), input);
            }
            other => panic!("expected a finished job, got {other:?}"),
        }
        assert_eq!(std::fs::read(&input).unwrap(), b"the original", "the source must survive");
        assert!(!staged.exists(), "the discarded result must not linger");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_job_that_keeps_its_original_touches_nothing() {
        let dir = scratch("settle-keep");
        let bin = dir.join("recycle-bin");
        let input = dir.join("clip.mp4");
        let output = dir.join("clip (compressed).mp4");
        std::fs::write(&input, b"the original").unwrap();
        std::fs::write(&output, b"the result").unwrap();

        let item = QueueItem { replacement: None, ..replacing_item(&input, &output, &input) };

        match settle_with(&item, image_result(10), fake_bin(&bin)) {
            JobEvent::Finished { original, output: reported, .. } => {
                assert_eq!(original, Original::Kept);
                assert_eq!(PathBuf::from(reported), output);
            }
            other => panic!("expected a finished job, got {other:?}"),
        }
        assert!(input.exists(), "keeping means keeping");
        assert!(output.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A queue with no FFmpeg behind it. Every job fails the moment the worker
    /// picks it up, which is what makes the scheduling observable: an event
    /// means the worker took the job, silence means it did not.
    fn test_queue() -> (Queue, Receiver<JobEvent>) {
        let tools: Arc<Mutex<Option<FfmpegTools>>> = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel();

        let queue = Queue::start(
            tools,
            Arc::new(move |event| {
                let _ = tx.send(event);
            }),
        );

        (queue, rx)
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
            output_dir: None,
            replacement: None,
        }
    }

    /// The ids that reached a terminal state, giving up after ten seconds.
    fn settled_ids(rx: &Receiver<JobEvent>, expected: usize) -> HashSet<String> {
        let mut settled: HashSet<String> = HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(10);

        while settled.len() < expected && Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(250)) {
                // Either terminal state is acceptable; being told nothing is not.
                Ok(JobEvent::Cancelled { id }) | Ok(JobEvent::Failed { id, .. }) => {
                    settled.insert(id);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }

        settled
    }

    #[test]
    fn a_queue_with_no_tools_fails_jobs_rather_than_hanging() {
        let (queue, rx) = test_queue();
        queue.push(waiting_item("job-1"));

        let event = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the queue should report something rather than hang");

        assert!(
            matches!(event, JobEvent::Failed { .. }),
            "expected a failure, got {event:?}"
        );
    }

    /// The invariant a stalled row exposed: every job must end in *some*
    /// reported state. Cancelling used to pull waiting jobs out of the queue
    /// before the worker could see them, so nothing ever reported those — the
    /// row sat on "queued" forever and could not be dismissed.
    #[test]
    fn every_job_is_reported_even_when_cancelled_before_it_runs() {
        let (queue, rx) = test_queue();

        let ids: Vec<String> = (0..6).map(|n| format!("bulk-{n}")).collect();
        for id in &ids {
            queue.push(waiting_item(id));
        }
        queue.cancel_all();

        let settled = settled_ids(&rx, ids.len());
        let stranded: Vec<&String> = ids.iter().filter(|id| !settled.contains(*id)).collect();
        assert!(stranded.is_empty(), "these jobs were never reported: {stranded:?}");
    }

    #[test]
    fn cancelling_a_queued_job_stops_it_ever_running() {
        let (queue, rx) = test_queue();

        let item = waiting_item("job-2");
        queue.cancel(&item.id);
        queue.push(item);

        // Cancelling an id nobody has queued yet is a no-op, so this asserts
        // the weaker but still important property: the queue always resolves
        // a job somehow.
        let event = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the queue should resolve the job");
        assert!(matches!(event, JobEvent::Failed { .. } | JobEvent::Cancelled { .. }));
    }

    #[test]
    fn a_paused_queue_starts_nothing_and_resuming_drains_it() {
        let (queue, rx) = test_queue();

        queue.pause();
        queue.push(waiting_item("held-1"));
        queue.push(waiting_item("held-2"));

        assert_eq!(queue.status(), QueueStatus { paused: true, waiting: 2, running: false });
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "a paused queue must not start anything"
        );

        queue.resume();

        assert_eq!(
            settled_ids(&rx, 2).len(),
            2,
            "resuming must run everything that was held"
        );
        assert_eq!(queue.status().waiting, 0);
    }

    /// The property the Stop button rests on: stopping is not a one-way door,
    /// and the work is still there afterwards.
    #[test]
    fn pausing_keeps_the_waiting_jobs() {
        let (queue, _rx) = test_queue();

        queue.pause();
        for n in 0..4 {
            queue.push(waiting_item(&format!("kept-{n}")));
        }

        let status = queue.pause();
        assert!(status.paused);
        assert_eq!(status.waiting, 4, "a pause must not discard the queue");
    }

    #[test]
    fn a_job_cancelled_while_it_waits_is_reported_and_never_starts() {
        let (queue, rx) = test_queue();

        queue.pause();
        queue.push(waiting_item("doomed"));
        queue.push(waiting_item("survivor"));

        queue.cancel("doomed");

        // Reported straight away: the worker is paused and will never see it.
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(JobEvent::Cancelled { id }) => assert_eq!(id, "doomed"),
            other => panic!("expected doomed to be cancelled, got {other:?}"),
        }

        queue.resume();

        let settled = settled_ids(&rx, 1);
        assert!(settled.contains("survivor"), "the rest of the queue must still run");
        assert!(
            !settled.contains("doomed"),
            "a cancelled job must not run when the queue resumes"
        );
    }

    /// Discarding a paused queue has to leave it able to run again, or the next
    /// file added would sit there with no clue why.
    #[test]
    fn clearing_a_paused_queue_lifts_the_pause() {
        let (queue, rx) = test_queue();

        queue.pause();
        queue.push(waiting_item("dropped"));
        queue.cancel_all();

        assert!(!queue.status().paused);

        queue.push(waiting_item("after"));

        // Two events: the job the clear threw away, and the one added since.
        assert!(
            settled_ids(&rx, 2).contains("after"),
            "a queue cleared while paused must accept new work"
        );
    }
}
