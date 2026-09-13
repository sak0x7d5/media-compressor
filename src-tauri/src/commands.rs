//! The surface the frontend can call, and the events it receives back.

use crate::clipboard;
use crate::ffmpeg::acquire::{self, AcquireProgress};
use crate::ffmpeg::encode::Speed;
use crate::ffmpeg::probe::probe;
use crate::ffmpeg::tools::FfmpegTools;
use crate::images::{is_image, ImageFormat};
use crate::presets::{self, PresetFile};
use crate::preview::{self, PreviewPair};
use crate::queue::{output_path_for, JobEvent, Queue, QueueItem};
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
    /// Only the Settings panel ever displays this, and asking for it means
    /// spawning a 100 MB binary, so it is resolved lazily and then kept.
    pub version: Arc<Mutex<Option<String>>>,
    pub queue: Queue,
    pub cache_dir: PathBuf,
    pub config_dir: PathBuf,
    pub work_root: PathBuf,
    /// Paths this process was launched with, consumed once by the frontend.
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

#[derive(Debug, Clone, Serialize)]
pub struct FfmpegStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub location: Option<String>,
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
}

fn default_margin() -> f64 {
    Target::DEFAULT_SAFETY_MARGIN
}

impl EncodeSettings {
    fn target(&self) -> Target {
        Target { limit_bytes: self.target_bytes, safety_margin: self.safety_margin }
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

/// Whether FFmpeg is present, and which build.
///
/// Async deliberately. A synchronous command body runs on the thread pumping
/// the window's messages, and resolving the version banner spawns FFmpeg — on
/// the first launch after a boot that is seconds of frozen UI for a string
/// only the Settings panel reads.
#[tauri::command]
pub async fn ffmpeg_status(app: AppHandle) -> FfmpegStatus {
    let cache = Arc::clone(&app.state::<AppState>().version);

    let Some(tools) = located_tools(&app) else {
        return FfmpegStatus { installed: false, version: None, location: None };
    };

    let location = tools.ffmpeg.to_string_lossy().to_string();
    let version = cached_version(&cache, tools).await;

    FfmpegStatus { installed: true, version, location: Some(location) }
}

/// The binaries this run located at startup, if it found any.
fn located_tools(app: &AppHandle) -> Option<FfmpegTools> {
    app.state::<AppState>().tools.lock().unwrap().clone()
}

/// The version banner, asked of the binary at most once per run.
async fn cached_version(cache: &Mutex<Option<String>>, tools: FfmpegTools) -> Option<String> {
    let known = cache.lock().unwrap().clone();
    if known.is_some() {
        return known;
    }

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

    let version = installed.version();
    let location = installed.ffmpeg.to_string_lossy().to_string();
    *tools_slot.lock().unwrap() = Some(installed);
    *version_slot.lock().unwrap() = version.clone();

    Ok(FfmpegStatus { installed: true, version, location: Some(location) })
}

/// Ask ffprobe what a file contains.
///
/// Async for the same reason as [`ffmpeg_status`]: the spawn must not land on
/// the thread pumping the window's messages.
#[tauri::command]
pub async fn probe_file(app: AppHandle, path: String) -> Result<MediaInfo, String> {
    let tools = located_tools(&app).ok_or("FFmpeg is not installed yet")?;

    tauri::async_runtime::spawn_blocking(move || probe(&tools, &PathBuf::from(path)))
        .await
        .map_err(|e| format!("probe task failed: {e}"))?
        .map_err(|e| e.to_string())
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
        let output = output_path_for(&input, extension, output_dir.as_deref());
        let input_bytes = std::fs::metadata(&input).map(|meta| meta.len()).unwrap_or(0);
        let name = input
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());

        let summary = QueuedFile {
            id: id.clone(),
            input: input.to_string_lossy().to_string(),
            output: output.to_string_lossy().to_string(),
            name,
            input_bytes,
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

/// Files this launch was handed on the command line.
///
/// The Explorer context menu starts us with paths in argv. The frontend asks
/// for them once, on mount; subsequent launches arrive as `open-files` events
/// through the single-instance plugin instead.
#[tauri::command]
pub fn pending_files(state: State<'_, AppState>) -> Vec<String> {
    std::mem::take(&mut *state.pending_files.lock().unwrap())
}

/// A matched pair of frames from the original and the result.
///
/// Extracting the two frames means running FFmpeg twice over files that may be
/// long, so this runs off the UI thread — otherwise every comparison froze the
/// window for as long as the seek took.
#[tauri::command]
pub async fn preview_pair(
    app: AppHandle,
    before: String,
    after: String,
    at_seconds: Option<f64>,
) -> Result<PreviewPair, String> {
    let tools = located_tools(&app).ok_or("FFmpeg is not installed yet")?;

    tauri::async_runtime::spawn_blocking(move || {
        preview::compare(&tools, &PathBuf::from(before), &PathBuf::from(after), at_seconds)
    })
    .await
    .map_err(|e| format!("preview task failed: {e}"))?
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn shell_menu_status() -> bool {
    shell_integration::is_registered()
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
