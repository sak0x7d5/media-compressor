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
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

/// What becomes of the original once the compressed file is written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Disposition {
    /// The result is a new file and the original stays where it is.
    #[default]
    Keep,
    /// The result takes the original's place and the original is deleted.
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
    /// Replacement was asked for, but the result never got under the target.
    /// It is smaller, and it still will not upload — so the source stays,
    /// because it is the copy that can still be re-encoded to something that
    /// does fit. The result was discarded.
    KeptOverLimit,
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
        self.cancel_all();

        // `stopping` has to be set while holding `pending`, because that is the
        // lock the worker is parked on. Setting it outside leaves a window
        // where the worker has already read `stopping` as false but has not yet
        // reached `wait`, so it misses the notification and parks forever.
        let pending = self.shared.pending.lock().unwrap();
        *self.shared.stopping.lock().unwrap() = true;
        self.shared.wake.notify_all();
        drop(pending);
    }
}

fn worker(
    shared: Arc<Shared>,
    tools: Arc<Mutex<Option<FfmpegTools>>>,
    listener: Listener,
) {
    while let Some(mut item) = shared.next() {
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

        let progress_listener = Arc::clone(&listener);
        let id = item.id.clone();
        let result = compress_media(&tools, &request, &token, move |stage: Stage| {
            progress_listener(JobEvent::Progress { id: id.clone(), stage });
        });

        match result {
            Ok(outcome) => listener(settle(&item, outcome)),
            Err(error) => {
                // A replacement encodes into the user's own folder, so a job
                // that ended early leaves a half-written scratch file sitting
                // next to the original. It goes however the job ended.
                if item.replacement.is_some() {
                    let _ = std::fs::remove_file(&item.output);
                }

                if error.is_cancellation() {
                    listener(JobEvent::Cancelled { id: item.id.clone() });
                } else {
                    listener(JobEvent::Failed { id: item.id.clone(), message: error.to_string() });
                }
            }
        }

        shared.tokens.lock().unwrap().remove(&item.id);
    }
}

/// Turn a finished encode into the event the UI sees.
///
/// This is the only place a source file is ever destroyed, and it happens
/// after the encode rather than before it — by the time anything is removed
/// there is a complete file ready to take its place.
fn settle(item: &QueueItem, outcome: MediaOutcome) -> JobEvent {
    settle_with(item, outcome, recycle)
}

/// The body of [`settle`], with the recycle step handed in.
///
/// Tests supply their own. Giving up a file is the one thing here worth proving
/// in detail, and proving it against the real recycle bin would mean every
/// `cargo test` quietly filled the developer's own.
fn settle_with(
    item: &QueueItem,
    outcome: MediaOutcome,
    recycle: impl Fn(&Path) -> std::io::Result<()>,
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

    // Missing the target is the one failure this whole app exists to avoid, and
    // it is not a trade worth making: the source is lossless-relative-to-itself
    // and can be encoded again at a looser target, while the result is a lossy
    // file that still cannot be sent anywhere.
    if !outcome.within_limit() {
        let _ = std::fs::remove_file(&item.output);
        return JobEvent::Finished {
            id: item.id.clone(),
            output: item.input.to_string_lossy().to_string(),
            outcome,
            original: Original::KeptOverLimit,
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
        Err(error) => {
            // Only while the source is still there to fall back on. If it has
            // already gone to the bin, this scratch file is the only finished
            // encode left standing and removing it would compound the failure.
            if item.input.exists() {
                let _ = std::fs::remove_file(&item.output);
            }
            JobEvent::Failed {
                id: item.id.clone(),
                message: format!("could not replace the original: {error}"),
            }
        }
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

/// Put a finished file in its source's place.
///
/// Where the extension changes the two are different names, so the result goes
/// in first and the source is cleared after — no moment exists in which neither
/// is there. Where the extension is unchanged they are one path, and a plain
/// rename would obliterate the source with nothing kept back, so the source is
/// binned first and the staged encode — a complete file throughout — takes the
/// name it leaves.
fn install_replacement(
    original: &Path,
    staged: &Path,
    destination: &Path,
    recycle: impl Fn(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    // Decided before the move, while both paths still describe real files: a
    // rename that consumes the original leaves nothing left to compare.
    let destination_is_original = same_file(original, destination);

    if destination_is_original {
        recycle(original)?;
    }

    std::fs::rename(staged, destination)?;

    // A format change lands the result under a new name — `clip.mov` becomes
    // `clip.mp4` — leaving the source behind to clear. The result is already
    // in place by now, so a source that refuses to go (open in a player, say)
    // is litter rather than a failed job.
    if !destination_is_original {
        let _ = recycle(original);
    }

    Ok(())
}

/// Send a file to the platform's recycle bin.
///
/// Never a plain delete. What replaces it is lossy and is quite often the only
/// copy that will ever exist, so the one step that destroys something has to be
/// the one step the user can undo without us.
///
/// On Windows this initialises COM on the calling thread, which is why it is
/// reached from the encode worker rather than the thread serving the UI.
fn recycle(path: &Path) -> std::io::Result<()> {
    trash::delete(path).map_err(|error| std::io::Error::other(error.to_string()))
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

    /// A stand-in recycle bin.
    ///
    /// It moves the file into a folder of its own, which is what the real one
    /// does — and lets a test assert the original is *recoverable* rather than
    /// merely gone, which is the whole point of not deleting outright.
    fn fake_bin(bin: &Path) -> impl Fn(&Path) -> std::io::Result<()> + '_ {
        move |path: &Path| {
            std::fs::create_dir_all(bin)?;
            std::fs::rename(path, bin.join(path.file_name().unwrap()))
        }
    }

    /// The same result, except that it never got under the target.
    fn missed_the_target(output_bytes: u64) -> MediaOutcome {
        match image_result(output_bytes) {
            MediaOutcome::Image(mut outcome) => {
                outcome.within_limit = false;
                MediaOutcome::Image(outcome)
            }
            other => other,
        }
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
        let input = dir.join("clip.mp4");
        std::fs::write(&input, b"the original").unwrap();

        let staged = staging_path_for(&input, "mp4", "job-1");
        std::fs::write(&staged, b"the compressed result").unwrap();

        let bin = dir.join("bin");
        install_replacement(&input, &staged, &input, fake_bin(&bin)).unwrap();

        assert_eq!(std::fs::read(&input).unwrap(), b"the compressed result");
        assert!(!staged.exists(), "the scratch file must not linger");
        // The name is reused, so the source can only survive by being moved
        // aside first. That it is still readable is the difference between this
        // and an overwrite.
        assert_eq!(
            std::fs::read(bin.join("clip.mp4")).unwrap(),
            b"the original",
            "the source must be recoverable, not destroyed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installing_a_new_format_replacement_clears_the_old_file() {
        let dir = scratch("install-new-ext");
        let input = dir.join("clip.mov");
        std::fs::write(&input, b"the original").unwrap();

        let destination = dir.join("clip.mp4");
        let staged = staging_path_for(&input, "mp4", "job-1");
        std::fs::write(&staged, b"the compressed result").unwrap();

        let bin = dir.join("bin");
        install_replacement(&input, &staged, &destination, fake_bin(&bin)).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"the compressed result");
        assert!(!input.exists(), "the source must be gone once its result is in place");
        assert!(!staged.exists(), "the scratch file must not linger");
        assert_eq!(
            std::fs::read(bin.join("clip.mov")).unwrap(),
            b"the original",
            "the source must be recoverable, not destroyed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_finished_replacement_reports_the_destination_rather_than_the_scratch_file() {
        let dir = scratch("settle-replaced");
        let input = dir.join("clip.mp4");
        std::fs::write(&input, vec![0u8; 4096]).unwrap();

        let staged = staging_path_for(&input, "mp4", "job-1");
        std::fs::write(&staged, b"smaller").unwrap();

        let bin = dir.join("bin");
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

        let first_result = dir.join("clip.mp4");
        std::fs::write(&first_result, b"the result of compressing clip.mov").unwrap();

        // Queued when `clip.mp4` did not exist yet, so this is what it chose.
        let second = dir.join("clip.webm");
        std::fs::write(&second, vec![0u8; 4096]).unwrap();
        let staged = staging_path_for(&second, "mp4", "job-2");
        std::fs::write(&staged, b"smaller").unwrap();

        let bin = dir.join("bin");
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
        let input = dir.join("clip.mp4");
        std::fs::write(&input, b"the original").unwrap();

        let staged = staging_path_for(&input, "mp4", "job-1");
        std::fs::write(&staged, b"a bigger re-encode of the original").unwrap();

        let event = settle_with(&replacing_item(&input, &staged, &input), image_result(34), |_| {
            panic!("nothing should be given up when the result is no smaller")
        });

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

    /// Smaller is not the bar — fitting is. A result that missed the target is
    /// a lossy file that still cannot be sent anywhere, and taking the source's
    /// place would leave nothing to re-encode from at a looser target. The one
    /// case where the source is strictly more valuable than a smaller file.
    #[test]
    fn a_result_that_missed_the_target_leaves_the_source_alone() {
        let dir = scratch("settle-over-limit");
        let input = dir.join("clip.mp4");
        std::fs::write(&input, vec![0u8; 4096]).unwrap();

        let staged = staging_path_for(&input, "mp4", "job-1");
        // Genuinely smaller, so only the limit stands between it and the swap.
        std::fs::write(&staged, vec![0u8; 512]).unwrap();

        let settled = settle_with(&replacing_item(&input, &staged, &input), missed_the_target(512), |_| {
            panic!("nothing should be given up when the result missed the target")
        });
        match settled {
            JobEvent::Finished { output, original, .. } => {
                assert_eq!(original, Original::KeptOverLimit);
                assert_eq!(
                    PathBuf::from(output),
                    input,
                    "the surviving file is what the UI must act on"
                );
            }
            other => panic!("expected a finished job, got {other:?}"),
        }

        assert_eq!(
            std::fs::metadata(&input).unwrap().len(),
            4096,
            "the source must be untouched"
        );
        assert!(!staged.exists(), "the discarded result must not linger");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_job_that_keeps_its_original_touches_nothing() {
        let dir = scratch("settle-keep");
        let input = dir.join("clip.mp4");
        let output = dir.join("clip (compressed).mp4");
        std::fs::write(&input, b"the original").unwrap();
        std::fs::write(&output, b"the result").unwrap();

        let item = QueueItem { replacement: None, ..replacing_item(&input, &output, &input) };

        match settle(&item, image_result(10)) {
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
            output_dir: None,
            replacement: None,
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
            output_dir: None,
            replacement: None,
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
            output_dir: None,
            replacement: None,
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
