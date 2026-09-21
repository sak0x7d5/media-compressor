/** Typed wrappers over the Rust command surface. */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export const EVENT_JOB = 'job';
export const EVENT_INSTALL = 'ffmpeg-install';
export const EVENT_OPEN_FILES = 'open-files';

export interface Preset {
	id: string;
	group: string;
	label: string;
	bytes: number;
	default?: boolean;
	note?: string;
}

export interface PresetFile {
	version: number;
	updated: string;
	presets: Preset[];
	source_url?: string | null;
	last_refreshed?: number | null;
}

/** Where the binaries in use came from. */
export type FfmpegSource = 'private' | 'override' | 'system';

export interface FfmpegStatus {
	installed: boolean;
	location: string | null;
	source: FfmpegSource | null;
}

/** An FFmpeg already on the user's PATH, and whether it is good enough. */
export interface SystemBuild {
	location: string;
	usable: boolean;
	/** Encoders this app needs that the build does not have. */
	missing_encoders: string[];
}

/**
 * What a launch was asked to do. `target_bytes` is set when the user picked a
 * size straight from the Explorer submenu instead of opening the app cold.
 */
export interface Launch {
	files: string[];
	target_bytes: number | null;
}

/** Everything the first frame needs, fetched in one round trip. */
export interface Startup {
	ffmpeg: FfmpegStatus;
	presets: PresetFile;
	/** What this launch was handed on the command line. */
	launch: Launch;
	/** Whether the Explorer right-click entry can be offered on this platform. */
	shell_supported: boolean;
}

export interface MediaInfo {
	duration_secs: number;
	width: number;
	height: number;
	fps: number;
	audio: { channels: number } | null;
}

export interface Scale {
	width: number;
	height: number;
	fps: number;
	bpp: number;
	below_floor: boolean;
}

export type RateControl =
	| { mode: 'crf'; crf: number }
	| { mode: 'two-pass'; video_bps: number };

export interface EncodePlan {
	codec: 'h264' | 'hevc' | 'av1';
	scale: Scale;
	rate_control: RateControl;
	audio_bps: number;
	attempt: number;
}

export interface EncodeProgress {
	pass: number;
	of_passes: number;
	pass_fraction: number;
	overall_fraction: number;
	fps: number;
	speed: number;
	output_bytes: number;
}

export type Stage =
	| { stage: 'probing' }
	| { stage: 'predicting' }
	| { stage: 'planned'; plan: EncodePlan; predicted_bytes: number | null }
	| ({ stage: 'encoding' } & EncodeProgress)
	| { stage: 'correcting'; attempt: number; actual_bytes: number }
	| {
			stage: 'searching';
			encode: number;
			of: number;
			quality: number;
			bytes: number;
			fits: boolean;
	  };

export interface VideoOutcome {
	kind: 'video';
	info: MediaInfo;
	plan: EncodePlan;
	output_bytes: number;
	attempts: number;
	within_limit: boolean;
}

export interface ImageOutcome {
	kind: 'image';
	output_bytes: number;
	quality: number;
	width: number;
	height: number;
	downscale_steps: number;
	encodes: number;
	within_limit: boolean;
}

/** Serde tags these on `kind`, flattening the variant's fields alongside it. */
export type MediaOutcome = VideoOutcome | ImageOutcome;

/** What became of the source file, once the job finished. */
export type Original = 'kept' | 'replaced' | 'kept-not-smaller';

export type JobEvent =
	| { event: 'queued'; id: string; input: string; output: string }
	| { event: 'started'; id: string }
	| { event: 'progress'; id: string; stage: Stage }
	| {
			event: 'finished';
			id: string;
			outcome: MediaOutcome;
			output: string;
			original: Original;
	  }
	| { event: 'failed'; id: string; message: string }
	| { event: 'cancelled'; id: string };

export type InstallProgress =
	| { step: 'starting' }
	| { step: 'downloading'; downloaded_bytes: number; total_bytes: number }
	| { step: 'unpacking' }
	| { step: 'verifying' }
	| { step: 'done' };

export interface EncodeSettings {
	target_bytes: number;
	safety_margin?: number;
	codec?: 'h264' | 'hevc' | 'av1';
	bias?: 'resolution-first' | 'balanced' | 'sharpness-first';
	speed?: 'fast' | 'balanced' | 'slow';
	crf?: number;
	image_format?: 'webp' | 'avif' | 'jpeg' | 'png';
	max_dimension?: number;
	/** Where results are written. Null or absent means beside the original. */
	output_dir?: string | null;
	/** Whether the result joins the original or takes its place. */
	disposition?: Disposition;
}

/** What happens to the original once the result is written. */
export type Disposition = 'keep' | 'replace';

/**
 * Where compressed files land. "replace" is the one answer that gives up a
 * file the user already had: the result takes the original's name and the
 * original goes to the recycle bin.
 */
export type OutputMode = 'beside' | 'folder' | 'ask' | 'replace';

/**
 * The Explorer right-click entry. `supported` is false off Windows, where the
 * control is hidden rather than shown as a switch that can only fail.
 */
export interface ShellMenuStatus {
	supported: boolean;
	enabled: boolean;
}

/**
 * What a launch was asked to do. `target_bytes` is set when the user picked a
 * size straight from the Explorer submenu instead of opening the app cold.
 */
export interface Launch {
	files: string[];
	target_bytes: number | null;
}

export interface QueuedFile {
	id: string;
	input: string;
	output: string;
	name: string;
	input_bytes: number;
	replaces_input: boolean;
}

/** The queue as a whole — what the Stop / Resume / Clear controls are about. */
export interface QueueStatus {
	paused: boolean;
	/** Jobs waiting for the worker. */
	waiting: number;
	running: boolean;
}

export interface UpdateInfo {
	version: string;
	current_version: string;
	notes: string | null;
	date: string | null;
}

export interface PreviewPair {
	before: string;
	after: string;
	at_seconds: number;
}

export const startup = () => invoke<Startup>('startup');

/**
 * Tell the backend the UI is populated, which is what makes the window
 * visible. The window is created hidden so nobody watches an empty frame while
 * the webview starts, so this must be called on every path out of boot,
 * including the failing ones.
 */
export const uiReady = () => invoke<void>('ui_ready');

export const listPresets = () => invoke<PresetFile>('list_presets');
export const savePresets = (presets: PresetFile) => invoke<void>('save_presets', { presets });
export const ffmpegStatus = () => invoke<FfmpegStatus>('ffmpeg_status');
/** Runs `ffmpeg -version`, so it is asked for lazily rather than at startup. */
export const ffmpegVersion = () => invoke<string | null>('ffmpeg_version');
/**
 * Download the app's own copy.
 *
 * `force` is the Settings escape hatch — "I know you found one on my PATH, I
 * want yours anyway". Left off, an existing copy is reused rather than
 * re-fetched.
 */
export const installFfmpeg = (force = false) => invoke<FfmpegStatus>('install_ffmpeg', { force });

/** Switch to the FFmpeg already on the machine, deleting the downloaded copy. */
export const useSystemFfmpeg = () => invoke<FfmpegStatus>('use_system_ffmpeg');

/** Costs a process the first time it is asked, so only Settings asks. */
export const systemFfmpeg = () => invoke<SystemBuild | null>('system_ffmpeg');
export const probeFile = (path: string) => invoke<MediaInfo>('probe_file', { path });
export const addFiles = (paths: string[], settings: EncodeSettings) =>
	invoke<QueuedFile[]>('add_files', { paths, settings });
export const cancelJob = (id: string) => invoke<void>('cancel_job', { id });
/** Throw the queue away. */
export const cancelAll = () => invoke<QueueStatus>('cancel_all');
/** Stop working, keeping the queue. The running file goes back in the queue. */
export const pauseQueue = () => invoke<QueueStatus>('pause_queue');
export const resumeQueue = () => invoke<QueueStatus>('resume_queue');
export const copyToClipboard = (paths: string[]) => invoke<void>('copy_to_clipboard', { paths });
export const revealInFolder = (path: string) => invoke<void>('reveal_in_folder', { path });

export const setPresetsUrl = (url: string | null) =>
	invoke<PresetFile>('set_presets_url', { url });
export const refreshPresets = (force = false) =>
	invoke<PresetFile | null>('refresh_presets', { force });

/** Resolves to null when already up to date; rejects when it couldn't find out. */
export const checkForUpdate = () => invoke<UpdateInfo | null>('check_for_update');
export const installUpdate = () => invoke<void>('install_update');

export const previewPair = (before: string, after: string, atSeconds?: number) =>
	invoke<PreviewPair>('preview_pair', { before, after, atSeconds });
export const shellMenuStatus = () => invoke<ShellMenuStatus>('shell_menu_status');
export const setShellMenu = (enabled: boolean) =>
	invoke<ShellMenuStatus>('set_shell_menu', { enabled });

export const onOpenFiles = (handler: (launch: Launch) => void): Promise<UnlistenFn> =>
	listen<Launch>(EVENT_OPEN_FILES, (message) => handler(message.payload));

export const onJobEvent = (handler: (event: JobEvent) => void): Promise<UnlistenFn> =>
	listen<JobEvent>(EVENT_JOB, (message) => handler(message.payload));

export const onInstallProgress = (handler: (event: InstallProgress) => void): Promise<UnlistenFn> =>
	listen<InstallProgress>(EVENT_INSTALL, (message) => handler(message.payload));
