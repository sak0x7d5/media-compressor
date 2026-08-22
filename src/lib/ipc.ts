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

export interface FfmpegStatus {
	installed: boolean;
	version: string | null;
	location: string | null;
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

export type JobEvent =
	| { event: 'queued'; id: string; input: string; output: string }
	| { event: 'started'; id: string }
	| { event: 'progress'; id: string; stage: Stage }
	| { event: 'finished'; id: string; outcome: MediaOutcome; output: string }
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
}

/** Where compressed files land. */
export type OutputMode = 'beside' | 'folder' | 'ask';

export interface QueuedFile {
	id: string;
	input: string;
	output: string;
	name: string;
	input_bytes: number;
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

export const listPresets = () => invoke<PresetFile>('list_presets');
export const savePresets = (presets: PresetFile) => invoke<void>('save_presets', { presets });
export const ffmpegStatus = () => invoke<FfmpegStatus>('ffmpeg_status');
export const installFfmpeg = () => invoke<FfmpegStatus>('install_ffmpeg');
export const probeFile = (path: string) => invoke<MediaInfo>('probe_file', { path });
export const addFiles = (paths: string[], settings: EncodeSettings) =>
	invoke<QueuedFile[]>('add_files', { paths, settings });
export const cancelJob = (id: string) => invoke<void>('cancel_job', { id });
export const cancelAll = () => invoke<void>('cancel_all');
export const copyToClipboard = (paths: string[]) => invoke<void>('copy_to_clipboard', { paths });
export const revealInFolder = (path: string) => invoke<void>('reveal_in_folder', { path });

export const setPresetsUrl = (url: string | null) =>
	invoke<PresetFile>('set_presets_url', { url });
export const refreshPresets = (force = false) =>
	invoke<PresetFile | null>('refresh_presets', { force });

/** Resolves to null when already up to date; rejects when it couldn't find out. */
export const checkForUpdate = () => invoke<UpdateInfo | null>('check_for_update');
export const installUpdate = () => invoke<void>('install_update');

export const pendingFiles = () => invoke<string[]>('pending_files');
export const previewPair = (before: string, after: string, atSeconds?: number) =>
	invoke<PreviewPair>('preview_pair', { before, after, atSeconds });
export const shellMenuStatus = () => invoke<boolean>('shell_menu_status');
export const setShellMenu = (enabled: boolean) => invoke<boolean>('set_shell_menu', { enabled });

export const onOpenFiles = (handler: (paths: string[]) => void): Promise<UnlistenFn> =>
	listen<string[]>(EVENT_OPEN_FILES, (message) => handler(message.payload));

export const onJobEvent = (handler: (event: JobEvent) => void): Promise<UnlistenFn> =>
	listen<JobEvent>(EVENT_JOB, (message) => handler(message.payload));

export const onInstallProgress = (handler: (event: InstallProgress) => void): Promise<UnlistenFn> =>
	listen<InstallProgress>(EVENT_INSTALL, (message) => handler(message.payload));
