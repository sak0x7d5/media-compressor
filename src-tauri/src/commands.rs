//! The surface the frontend can call, and the events it receives back.

use crate::clipboard;
use crate::ffmpeg::acquire::{self, AcquireProgress};
use crate::ffmpeg::encode::Speed;
use crate::ffmpeg::probe::probe;
use crate::ffmpeg::tools::FfmpegTools;
use crate::images::{is_image, ImageFormat};
use crate::presets::{self, PresetFile};
use crate::preview::{self, PreviewPair};
use crate::queue::{
    output_path_for, replacement_path_for, staging_path_for, Disposition, JobEvent, Queue,
    QueueItem,
};
use crate::shell_integration;
use crate::strategy::plan::Options;
use crate::strategy::{MediaInfo, SharpnessBias, Target, VideoCodec};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

/// Event channel names. Kept in one place so the frontend and backend cannot
/// drift apart silently.
pub const EVENT_JOB: &str = "job";
pub const EVENT_INSTALL: &str = "ffmpeg-install";

pub struct AppState {
    pub tools: Arc<Mutex<Option<FfmpegTools>>>,
    /// The `ffmpeg -version` banner, once something has paid for it.
    ///
    /// Only the Settings panel ever displays this, and reading it means
    /// spawning a 100 MB binary, so it is resolved lazily and then kept.
    pub version: Arc<Mutex<Option<String>>>,
    pub queue: Queue,
    pub cache_dir: PathBuf,
    pub config_dir: PathBuf,
    pub work_root: PathBuf,
    /// Paths this process was launched with, consumed once by [`startup`].
    pub pending_files: Mutex<Vec<String>>,
    next_id: AtomicU64,
}

impl AppState {
    pub fn new(app: &AppHandle) -> Self {
        let paths = app.path();
        let cache_dir = paths
            .app_local_data_dir()
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("ffmpeg");
        let config_dir = paths.app_config_dir().unwrap_or_else(|_| std::env::temp_dir());
        let work_root = paths
            .app_local_data_dir()
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("work");

        let tools = Arc::new(Mutex::new(FfmpegTools::locate(&cache_dir).ok()));

        let emitter = app.clone();
        let queue = Queue::start(
            Arc::clone(&tools),
            Arc::new(move |event: JobEvent| {
                let _ = emitter.emit(EVENT_JOB, event);
            }),
        );

        Self {
            tools,
            version: Arc::new(Mutex::new(None)),
            queue,
            cache_dir,
            config_dir,
            work_root,
            pending_files: Mutex::new(crate::collect_file_args(
                &std::env::args().collect::<Vec<_>>(),
            )),
            next_id: AtomicU64::new(1),
        }
    }

    fn new_id(&self) -> String {
        format!("job-{}", self.next_id.fetch_add(1, Ordering::SeqCst))
    }
}

/// Whether FFmpeg is here, and where.
///
/// Deliberately says nothing about *which* build it is: answering that means
/// running `ffmpeg -version`, and this is the first thing startup asks for.
/// See [`ffmpeg_version`].
#[derive(Debug, Clone, Serialize)]
pub struct FfmpegStatus {
    pub installed: bool,
    pub location: Option<String>,
}

impl FfmpegStatus {
    fn of(tools: Option<&FfmpegTools>) -> Self {
        match tools {
            Some(tools) => Self {
                installed: true,
                location: Some(tools.ffmpeg.to_string_lossy().to_string()),
            },
            None => Self { installed: false, location: None },
        }
    }
}

/// Everything the first frame needs, in one call.
#[derive(Debug, Clone, Serialize)]
pub struct Startup {
    pub ffmpeg: FfmpegStatus,
    pub presets: PresetFile,
    /// Paths this launch was handed on the command line — the Explorer context
    /// menu on a cold start. Consumed here, so this is the only place that
    /// will ever report them.
    pub pending_files: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EncodeSettings {
    pub target_bytes: u64,
    #[serde(default = "default_margin")]
    pub safety_margin: f64,
    #[serde(default)]
    pub codec: Option<VideoCodec>,
    #[serde(default)]
    pub bias: Option<SharpnessBias>,
    #[serde(default)]
    pub speed: Option<Speed>,
    #[serde(default)]
    pub crf: Option<u8>,
    /// Output format for still images. Ignored for video.
    #[serde(default)]
    pub image_format: Option<ImageFormat>,
    #[serde(default)]
    pub max_dimension: Option<u32>,
    /// Where results are written. `None` means beside the original.
    #[serde(default)]
    pub output_dir: Option<String>,
    /// Whether the result joins the original or takes its place. Defaults to
    /// joining it — the destructive answer is never the one nobody chose.
    #[serde(default)]
    pub disposition: Option<Disposition>,
}

fn default_margin() -> f64 {
    Target::DEFAULT_SAFETY_MARGIN
}

/// The narrowest and widest margins worth honouring.
///
/// Above 1.0 the app would plan *past* the very limit it exists to respect;
/// below 0.5 it would throw away more than half the budget. Both are more
/// likely to be a bad value than a real intention.
const MIN_SAFETY_MARGIN: f64 = 0.5;
const MAX_SAFETY_MARGIN: f64 = 1.0;

impl EncodeSettings {
    fn target(&self) -> Target {
        // This number crosses from the frontend, so it is checked rather than
        // trusted: a NaN would floor to a zero-byte budget and fail every job
        // with "target too small", and anything over 1.0 would quietly aim
        // above the cap and hand back a file the platform rejects.
        let safety_margin = if self.safety_margin.is_finite() {
            self.safety_margin.clamp(MIN_SAFETY_MARGIN, MAX_SAFETY_MARGIN)
        } else {
            Target::DEFAULT_SAFETY_MARGIN
        };

        Target { limit_bytes: self.target_bytes, safety_margin }
    }

    fn options(&self) -> Options {
        Options {
            codec: self.codec.unwrap_or(VideoCodec::H264),
            bias: self.bias.unwrap_or_default(),
            crf: self.crf,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct QueuedFile {
    pub id: String,
    pub input: String,
    pub output: String,
    pub name: String,
    pub input_bytes: u64,
    /// True when finishing this job will delete `input`. The UI shows the row
    /// differently for it, so it has to know before the encode rather than
    /// after.
    pub replaces_input: bool,
}

#[tauri::command]
pub fn list_presets(state: State<'_, AppState>) -> PresetFile {
    presets::load(&state.config_dir)
}

#[tauri::command]
pub fn save_presets(state: State<'_, AppState>, presets: PresetFile) -> Result<(), String> {
    crate::presets::save(&state.config_dir, &presets).map_err(|e| e.to_string())
}

/// Point the app at a URL to keep the preset list current, or `None` to stop.
///
/// With no URL configured — the default — the app never makes a network
/// request for presets.
#[tauri::command]
pub fn set_presets_url(state: State<'_, AppState>, url: Option<String>) -> Result<PresetFile, String> {
    let mut presets = presets::load(&state.config_dir);

    let trimmed = url.map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
    if let Some(value) = &trimmed {
        if !value.starts_with("https://") {
            return Err("the preset URL must start with https://".to_string());
        }
    }

    presets.source_url = trimmed;
    // Changing the source invalidates when we last checked it.
    presets.last_refreshed = None;
    presets::save(&state.config_dir, &presets).map_err(|e| e.to_string())?;

    Ok(presets)
}

/// Re-fetch the preset list now, if a URL is configured.
#[tauri::command]
pub async fn refresh_presets(app: AppHandle, force: bool) -> Result<Option<PresetFile>, String> {
    let config_dir = app.state::<AppState>().config_dir.clone();
    tauri::async_runtime::spawn_blocking(move || presets::refresh(&config_dir, force))
        .await
        .map_err(|e| format!("refresh task failed: {e}"))?
}

/// The located binaries, or the error the frontend shows when there are none.
///
/// Scoped so the mutex guard is released before any `.await` — holding a
/// `std::sync::MutexGuard` across an await point is how an async command
/// deadlocks itself.
fn resolve_tools(app: &AppHandle) -> Result<FfmpegTools, String> {
    let state = app.state::<AppState>();
    let guard = state.tools.lock().unwrap();
    guard.clone().ok_or_else(|| "FFmpeg is not installed yet".to_string())
}

/// Anything that shells out runs on a blocking thread rather than the main one.
///
/// Tauri runs a synchronous command on the main thread, which is also the
/// thread that paints the window. Pulling a frame out of a large file takes
/// long enough that doing it there freezes the UI until the child process
/// finishes.
async fn off_thread<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| format!("the task could not be run: {e}"))?
}

/// Whether FFmpeg is present, and where.
///
/// Synchronous on purpose: it reads a lock and formats a path, with no process
/// spawn anywhere in it, so there is nothing here worth taking off the UI
/// thread. The version banner — the one expensive part — is
/// [`ffmpeg_version`], which startup never asks for.
#[tauri::command]
pub fn ffmpeg_status(state: State<'_, AppState>) -> FfmpegStatus {
    FfmpegStatus::of(state.tools.lock().unwrap().as_ref())
}

/// The FFmpeg version banner, for the line at the bottom of Settings.
///
/// This spawns `ffmpeg -version` and waits for it, which on Windows costs
/// anywhere from tens of milliseconds to several seconds the first time, once
/// the antivirus has had its look at a freshly downloaded binary. So it runs
/// off the UI thread and nothing on the startup path asks for it.
///
/// The answer cannot change while the app runs — the binary is replaced only
/// by an install, which seeds the cache itself — so it is paid for once and
/// then remembered, rather than on every visit to Settings.
#[tauri::command]
pub async fn ffmpeg_version(app: AppHandle) -> Option<String> {
    let cache = Arc::clone(&app.state::<AppState>().version);

    if let Some(known) = cache.lock().unwrap().clone() {
        return Some(known);
    }

    let tools = {
        let state = app.state::<AppState>();
        let located = state.tools.lock().unwrap().clone();
        located?
    };

    let resolved = tauri::async_runtime::spawn_blocking(move || tools.version()).await.ok()??;
    *cache.lock().unwrap() = Some(resolved.clone());
    Some(resolved)
}

/// Download FFmpeg if it isn't already present.
///
/// Runs off the UI thread; progress arrives on the `ffmpeg-install` channel.
#[tauri::command]
pub async fn install_ffmpeg(app: AppHandle) -> Result<FfmpegStatus, String> {
    let state = app.state::<AppState>();
    let cache_dir = state.cache_dir.clone();
    let tools_slot = Arc::clone(&state.tools);
    let version_slot = Arc::clone(&state.version);

    let emitter = app.clone();
    let installed = tauri::async_runtime::spawn_blocking(move || {
        acquire::ensure(&cache_dir, move |progress: AcquireProgress| {
            let _ = emitter.emit(EVENT_INSTALL, progress);
        })
    })
    .await
    .map_err(|e| format!("install task failed: {e}"))?
    .map_err(|e| e.to_string())?;

    let location = installed.ffmpeg.to_string_lossy().to_string();
    *tools_slot.lock().unwrap() = Some(installed);
    // A different binary is on disk now, so whatever banner was remembered for
    // the last one no longer describes it. Cleared rather than re-read: only
    // Settings wants the string, and it will ask when it is opened.
    *version_slot.lock().unwrap() = None;

    Ok(FfmpegStatus { installed: true, location: Some(location) })
}

#[tauri::command]
pub async fn probe_file(app: AppHandle, path: String) -> Result<MediaInfo, String> {
    let tools = resolve_tools(&app)?;
    off_thread(move || probe(&tools, &PathBuf::from(path)).map_err(|e| e.to_string())).await
}

/// Queue one or more files for compression.
#[tauri::command]
pub fn add_files(
    state: State<'_, AppState>,
    app: AppHandle,
    paths: Vec<String>,
    settings: EncodeSettings,
) -> Vec<QueuedFile> {
    let mut queued = Vec::with_capacity(paths.len());

    // Resolved once: a folder that has gone missing since it was chosen falls
    // back to writing beside the original rather than failing every job.
    let output_dir = settings
        .output_dir
        .as_ref()
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir() || std::fs::create_dir_all(dir).is_ok());

    // Replacing happens in the source's own folder by definition, so a chosen
    // output folder has nothing to say about it.
    let disposition = settings.disposition.unwrap_or_default();

    for path in paths {
        let input = PathBuf::from(&path);
        if !input.is_file() {
            continue;
        }

        let id = state.new_id();
        // A PNG compressed for Discord comes back as WebP, so the output
        // extension follows the chosen format rather than the input.
        let image_format = settings.image_format.unwrap_or_default();
        let extension =
            if is_image(&input) { image_format.extension() } else { "mp4" };

        // Two paths, and in "keep" mode they are the same one: where FFmpeg
        // writes, and where the file ends up. Replacing separates them — the
        // encode goes to a scratch file first, and only a finished encode is
        // moved onto the original.
        let (output, destination, replacement) = match disposition {
            Disposition::Keep => {
                let output = output_path_for(&input, extension, output_dir.as_deref());
                (output.clone(), output, None)
            }
            Disposition::Replace => {
                let destination = replacement_path_for(&input, extension);
                let staging = staging_path_for(&input, extension, &id);
                (staging, destination.clone(), Some(destination))
            }
        };

        let input_bytes = std::fs::metadata(&input).map(|meta| meta.len()).unwrap_or(0);
        let name = input
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());

        let summary = QueuedFile {
            id: id.clone(),
            input: input.to_string_lossy().to_string(),
            // Where the file will end up, not the scratch file it passes
            // through. A row showing ".clip.job-3.part.mp4" would be nonsense.
            output: destination.to_string_lossy().to_string(),
            name,
            input_bytes,
            replaces_input: replacement.is_some(),
        };

        let _ = app.emit(
            EVENT_JOB,
            JobEvent::Queued {
                id: id.clone(),
                input: summary.input.clone(),
                output: summary.output.clone(),
            },
        );

        state.queue.push(QueueItem {
            id: id.clone(),
            input,
            output,
            target: settings.target(),
            options: settings.options(),
            speed: settings.speed.unwrap_or_default(),
            image_format,
            max_dimension: settings.max_dimension,
            // Carried through so the worker can resolve the path again once
            // the jobs ahead of this one have actually written their results.
            // Only a kept result is resolved again: a replacement stages under
            // a name of its own and settles its destination after the encode.
            output_dir: match disposition {
                Disposition::Keep => output_dir.clone(),
                Disposition::Replace => None,
            },
            replacement,
            // Each job gets its own scratch directory so two-pass logs from one
            // can never be picked up by another.
            work_dir: state.work_root.join(&id),
        });

        queued.push(summary);
    }

    queued
}

#[tauri::command]
pub fn cancel_job(state: State<'_, AppState>, id: String) {
    state.queue.cancel(&id);
}

#[tauri::command]
pub fn cancel_all(state: State<'_, AppState>) {
    state.queue.cancel_all();
}

#[tauri::command]
pub fn copy_to_clipboard(paths: Vec<String>) -> Result<(), String> {
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    clipboard::copy_files(&paths).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn reveal_in_folder(path: String) -> Result<(), String> {
    clipboard::reveal(&PathBuf::from(path)).map_err(|e| e.to_string())
}

/// A matched pair of frames from the original and the result.
///
/// This decodes up to two seek points in two files, which on a long clip is
/// comfortably long enough to be noticed as a freeze if it ran on the main
/// thread.
#[tauri::command]
pub async fn preview_pair(
    app: AppHandle,
    before: String,
    after: String,
    at_seconds: Option<f64>,
) -> Result<PreviewPair, String> {
    let tools = resolve_tools(&app)?;
    off_thread(move || {
        preview::compare(&tools, &PathBuf::from(before), &PathBuf::from(after), at_seconds)
            .map_err(|e| e.to_string())
    })
    .await
}

/// Reads the registry, so it runs off the UI thread like everything else that
/// touches the outside world.
#[tauri::command]
pub async fn shell_menu_status() -> bool {
    tauri::async_runtime::spawn_blocking(shell_integration::is_registered).await.unwrap_or(false)
}

/// Everything the first frame needs, in a single round trip.
///
/// These were four separate invokes awaited one after another, which meant the
/// window could not draw a populated UI until four IPC round trips had
/// completed in sequence. They are independent, cheap and always all wanted, so
/// they travel together.
#[tauri::command]
pub fn startup(state: State<'_, AppState>) -> Startup {
    // The first IPC of the run: everything before this is webview boot.
    crate::trace::mark("frontend: asked for startup state");

    Startup {
        ffmpeg: FfmpegStatus::of(state.tools.lock().unwrap().as_ref()),
        presets: presets::load(&state.config_dir),
        pending_files: std::mem::take(&mut *state.pending_files.lock().unwrap()),
    }
}

/// The frontend has a UI worth looking at; show the window.
///
/// The window starts hidden so that nobody watches an empty frame while
/// WebView2 boots. Calling this is what ends that — see
/// [`crate::reveal_main_window`], which also has the timeout that covers a
/// frontend that never gets this far.
#[tauri::command]
pub fn ui_ready(app: AppHandle) {
    crate::trace::mark("frontend: reported ready");
    crate::reveal_main_window(&app);
}

/// Add or remove the Explorer right-click entry.
///
/// Writes only under HKEY_CURRENT_USER, so this never needs administrator
/// rights and never affects other accounts on the machine.
#[tauri::command]
pub fn set_shell_menu(enabled: bool) -> Result<bool, String> {
    let result = if enabled {
        shell_integration::register()
    } else {
        shell_integration::unregister()
    };

    result.map_err(|e| e.to_string())?;
    Ok(shell_integration::is_registered())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(safety_margin: f64) -> EncodeSettings {
        EncodeSettings {
            target_bytes: 20_000_000,
            safety_margin,
            codec: None,
            bias: None,
            speed: None,
            crf: None,
            image_format: None,
            max_dimension: None,
            output_dir: None,
            disposition: None,
        }
    }

    #[test]
    fn an_ordinary_margin_is_passed_through_untouched() {
        assert_eq!(settings(0.95).target().safety_margin, 0.95);
        assert_eq!(settings(0.8).target().safety_margin, 0.8);
    }

    #[test]
    fn a_margin_over_one_would_aim_past_the_cap_and_is_pulled_back() {
        let target = settings(1.5).target();
        assert_eq!(target.safety_margin, MAX_SAFETY_MARGIN);
        assert!(
            target.effective_bytes() <= target.limit_bytes,
            "the planned size must never exceed the limit itself"
        );
    }

    #[test]
    fn a_margin_that_throws_away_most_of_the_budget_is_pulled_up() {
        assert_eq!(settings(0.01).target().safety_margin, MIN_SAFETY_MARGIN);
        assert_eq!(settings(0.0).target().safety_margin, MIN_SAFETY_MARGIN);
        assert_eq!(settings(-3.0).target().safety_margin, MIN_SAFETY_MARGIN);
    }

    #[test]
    fn a_non_finite_margin_falls_back_rather_than_zeroing_the_budget() {
        // `NaN as u64` floors to 0, which would fail every job with
        // "target too small" and give no clue why.
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let target = settings(value).target();
            assert_eq!(target.safety_margin, Target::DEFAULT_SAFETY_MARGIN);
            assert!(target.effective_bytes() > 0, "{value} produced an empty budget");
        }
    }
}
