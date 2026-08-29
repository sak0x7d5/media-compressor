<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { getCurrentWebview } from '@tauri-apps/api/webview';
	import { open } from '@tauri-apps/plugin-dialog';
	import type { UnlistenFn } from '@tauri-apps/api/event';

	import ComparePreview from '$lib/components/ComparePreview.svelte';
	import DropZone from '$lib/components/DropZone.svelte';
	import FileRow from '$lib/components/FileRow.svelte';
	import FirstRun from '$lib/components/FirstRun.svelte';
	import ResultCard from '$lib/components/ResultCard.svelte';
	import Settings from '$lib/components/Settings.svelte';
	import TargetPicker from '$lib/components/TargetPicker.svelte';

	import { JobList, type Job } from '$lib/jobs.svelte';
	import { formatBytes } from '$lib/format';
	import {
		addFiles,
		cancelAll,
		cancelJob,
		copyToClipboard,
		installFfmpeg,
		onInstallProgress,
		onJobEvent,
		onOpenFiles,
		previewPair,
		revealInFolder,
		startup,
		systemFfmpeg,
		useSystemFfmpeg,
		uiReady,
		type EncodeSettings,
		type FfmpegStatus,
		type InstallProgress,
		type OutputMode,
		type PresetFile,
		type PreviewPair,
		type SystemBuild
	} from '$lib/ipc';

	const CUSTOM = '__custom__';
	/* Must stay in step with EXTENSIONS in shell_integration.rs: anything the
	   Explorer menu offers has to be accepted here, or right-clicking a file the
	   app itself advertised ends in "that file type is not supported". */
	const MEDIA_EXTENSIONS = [
		'mp4', 'mov', 'mkv', 'webm', 'avi', 'm4v', 'wmv', 'flv', 'mpg', 'mpeg', 'ts', 'gif',
		'png', 'jpg', 'jpeg', 'webp', 'avif', 'bmp', 'tif', 'tiff'
	];

	const jobs = new JobList();

	let status = $state<FfmpegStatus | null>(null);
	/* Only consulted when there is nothing installed, to explain why an FFmpeg
	   the user can see on their PATH is not the one about to be used. The
	   backend decided this at startup and cached it, so asking is free. */
	let systemBuild = $state<SystemBuild | null>(null);
	let presets = $state<PresetFile | null>(null);
	let selectedId = $state('');
	let customBytes = $state(20_000_000);

	let installing = $state(false);
	let installProgress = $state<InstallProgress | null>(null);
	let installError = $state<string | null>(null);

	let hovering = $state(false);
	let toast = $state<string | null>(null);
	let toastTimer: ReturnType<typeof setTimeout> | undefined;

	let showSettings = $state(false);

	/* Where results go. "beside" writes next to the original, which is the
	   default and needs no folder at all. */
	let outputMode = $state<OutputMode>('beside');
	/* Replacing is the one mode that gives up a file the user already had, so it
	   stays visible in the footer rather than only in the settings panel it was
	   set from. */
	const replacing = $derived(outputMode === 'replace');
	let outputDir = $state<string | null>(null);
	let preview = $state<PreviewPair | null>(null);
	let previewBusy = $state(false);
	/* The clip the open comparison came from, kept so that scrubbing can go
	   back to the same two files without depending on the job still being
	   the one on screen. Duration is zero for a still, which has no
	   timeline. */
	let previewSource = $state<{ input: string; output: string; duration: number } | null>(null);
	let previewSeeking = $state(false);
	let seekToken = 0;
	let seekTimer: ReturnType<typeof setTimeout> | undefined;

	let options = $state<Omit<EncodeSettings, 'target_bytes'>>({
		safety_margin: 0.95,
		codec: 'h264',
		bias: 'balanced',
		speed: 'balanced',
		image_format: 'webp'
	});

	const limitBytes = $derived(
		selectedId === CUSTOM
			? customBytes
			: (presets?.presets.find((preset) => preset.id === selectedId)?.bytes ?? customBytes)
	);

	const settings = $derived<EncodeSettings>({ ...options, target_bytes: limitBytes });
	const soleResult = $derived(jobs.soleResult);

	/* A finished row can be opened to get the same result card a single file
	   gets. Only finished jobs have a result to show, and the selection is
	   dropped the moment that job stops existing. */
	let openJobId = $state<string | null>(null);
	const selectedJob = $derived(
		openJobId
			? jobs.jobs.find((job) => job.id === openJobId && job.state === 'done')
			: undefined
	);
	const detailJob = $derived(selectedJob ?? soleResult);
	const canGoBack = $derived(Boolean(selectedJob) && jobs.jobs.length > 1);

	function flash(message: string) {
		toast = message;
		clearTimeout(toastTimer);
		toastTimer = setTimeout(() => (toast = null), 2600);
	}

	function looksLikeMedia(path: string): boolean {
		const extension = path.split('.').pop()?.toLowerCase() ?? '';
		return MEDIA_EXTENSIONS.includes(extension);
	}

	/**
	 * The folder for this batch, asking first when that is the chosen mode.
	 * Returns undefined when the user cancels, which must abort the whole add
	 * rather than silently falling back to writing beside the originals.
	 */
	async function resolveOutputDir(): Promise<string | null | undefined> {
		if (outputMode === 'beside') return null;
		// Replacing happens in the original's own folder by definition, so there
		// is nothing to choose.
		if (outputMode === 'replace') return null;
		if (outputMode === 'folder') return outputDir;

		const chosen = await open({ directory: true, title: 'Save compressed files to' });
		if (!chosen) return undefined;
		return Array.isArray(chosen) ? chosen[0] : chosen;
	}

	async function enqueue(paths: string[]) {
		const media = paths.filter(looksLikeMedia);
		if (media.length === 0) {
			if (paths.length > 0) flash('That file type is not supported');
			return;
		}

		const dir = await resolveOutputDir();
		if (dir === undefined) return;

		preview = null;
		openJobId = null;
		try {
			jobs.add(
				await addFiles(media, {
					...settings,
					output_dir: dir,
					disposition: replacing ? 'replace' : 'keep'
				})
			);
		} catch (error) {
			flash(String(error));
		}
	}

	async function browse() {
		const chosen = await open({
			multiple: true,
			filters: [{ name: 'Media', extensions: MEDIA_EXTENSIONS }]
		});
		if (!chosen) return;
		await enqueue(Array.isArray(chosen) ? chosen : [chosen]);
	}

	async function install() {
		installing = true;
		installError = null;
		try {
			status = await installFfmpeg();
		} catch (error) {
			installError = String(error);
		} finally {
			installing = false;
		}
	}

	/** Offered only when startup found a usable build but stopped waiting on it. */
	async function adoptSystem() {
		installing = true;
		installError = null;
		try {
			status = await useSystemFfmpeg();
		} catch (error) {
			installError = String(error);
		} finally {
			installing = false;
		}
	}

	async function copy(path: string) {
		try {
			await copyToClipboard([path]);
			flash('Copied — paste it into Discord');
		} catch (error) {
			flash(String(error));
		}
	}

	async function copyAllFinished() {
		const paths = jobs.finished.map((job) => job.output);
		if (paths.length === 0) return;
		try {
			await copyToClipboard(paths);
			flash(paths.length === 1 ? 'Copied' : `Copied ${paths.length} files`);
		} catch (error) {
			flash(String(error));
		}
	}

	async function compare(job: Job) {
		previewBusy = true;
		try {
			preview = await previewPair(job.input, job.output);
			previewSource = {
				input: job.input,
				output: job.output,
				duration: job.outcome?.kind === 'video' ? job.outcome.info.duration_secs : 0
			};
		} catch (error) {
			flash(String(error));
		} finally {
			previewBusy = false;
		}
	}

	/**
	 * Move the comparison to another moment in the clip.
	 *
	 * Each seek is two ffmpeg runs against files on disk, so it waits for the
	 * drag to settle rather than firing per pixel — a swap mid-drag is a frame
	 * nobody looks at anyway.
	 */
	function seekPreview(seconds: number) {
		clearTimeout(seekTimer);
		if (previewSource) previewSeeking = true;
		seekTimer = setTimeout(() => void runSeek(seconds), 150);
	}

	/* Replies can land out of order, so a stale one is dropped rather than
	   allowed to paint over a newer frame. */
	async function runSeek(seconds: number) {
		const source = previewSource;
		if (!source) {
			previewSeeking = false;
			return;
		}

		const token = ++seekToken;
		previewSeeking = true;
		try {
			const pair = await previewPair(source.input, source.output, seconds);
			if (token === seekToken) preview = pair;
		} catch (error) {
			if (token === seekToken) flash(String(error));
		} finally {
			if (token === seekToken) previewSeeking = false;
		}
	}

	function closePreview() {
		clearTimeout(seekTimer);
		seekToken++;
		preview = null;
		previewSource = null;
		previewSeeking = false;
	}

	/**
	 * Fill the UI in and show the window.
	 *
	 * The window is created hidden, so this is the only thing that makes the app
	 * appear — which is why the reveal sits in `finally` and is awaited on a
	 * settled DOM. A backend that failed to answer must still leave the user
	 * with a window, and one that answered must not be shown mid-populate.
	 */
	async function boot() {
		let launchedWith: string[] = [];

		try {
			const initial = await startup();
			status = initial.ffmpeg;
			presets = initial.presets;
			launchedWith = initial.pending_files;

			const fallback = presets.presets.find((preset) => preset.default) ?? presets.presets[0];
			if (fallback) {
				selectedId = fallback.id;
				customBytes = fallback.bytes;
			}
		} catch (error) {
			flash(String(error));
		} finally {
			await tick();
			void uiReady();
		}

		// After the reveal: the first-run screen is worth a sentence about the
		// FFmpeg already on the machine, but not a delay before the window.
		if (status && !status.installed) {
			try {
				systemBuild = await systemFfmpeg();
			} catch {
				// Nothing to say about PATH is the same as having nothing to say.
			}
		}

		// Files handed to us on the command line — the Explorer context menu
		// path for a cold start. Queued after the reveal so a folder prompt has
		// a window to sit in front of.
		if (launchedWith.length > 0) await enqueue(launchedWith);
	}

	onMount(() => {
		const unlisteners: Promise<UnlistenFn>[] = [];

		unlisteners.push(onJobEvent((event) => jobs.apply(event)));
		unlisteners.push(onInstallProgress((event) => (installProgress = event)));
		// A second launch forwards its files here rather than opening a window.
		unlisteners.push(onOpenFiles((paths) => void enqueue(paths)));

		// Tauri delivers OS drag-and-drop to the webview rather than as DOM
		// events, so the browser's own dragover/drop never fire here.
		unlisteners.push(
			getCurrentWebview().onDragDropEvent((event) => {
				if (event.payload.type === 'over') hovering = true;
				else if (event.payload.type === 'leave') hovering = false;
				else if (event.payload.type === 'drop') {
					hovering = false;
					void enqueue(event.payload.paths);
				}
			})
		);

		void boot();

		return () => {
			clearTimeout(toastTimer);
			for (const pending of unlisteners) {
				void pending.then((unlisten) => unlisten());
			}
		};
	});
</script>

<main>
	{#if status && !status.installed}
		<FirstRun
			progress={installProgress}
			error={installError}
			busy={installing}
			system={systemBuild}
			onInstall={install}
			onUseSystem={adoptSystem}
		/>
	{:else}
		<header>
			<span class="count">
				{jobs.jobs.length === 0
					? 'No files'
					: `${jobs.jobs.length} ${jobs.jobs.length === 1 ? 'file' : 'files'}`}
			</span>
			<div class="target">
				<TargetPicker
					{presets}
					{selectedId}
					{customBytes}
					onSelect={(id) => (selectedId = id)}
					onCustom={(bytes) => (customBytes = bytes)}
				/>
			</div>
			<button
				class="icon"
				class:active={showSettings}
				onclick={() => (showSettings = !showSettings)}
				aria-label="Settings"
				title="Settings"
			>
				⚙
			</button>
		</header>

		<section
			class="body"
			class:empty={jobs.jobs.length === 0 && !showSettings}
			class:fill={Boolean(preview) && !showSettings}
		>
			{#if showSettings}
				<Settings
					settings={{ ...options, target_bytes: limitBytes }}
					{status}
					presetsSourceUrl={presets?.source_url ?? ''}
					{outputMode}
					{outputDir}
					onOutput={(mode, dir) => {
						outputMode = mode;
						outputDir = dir;
					}}
					onChange={(patch) => (options = { ...options, ...patch })}
					onPresets={(next) => (presets = next)}
					onFfmpeg={(next) => (status = next)}
					onClose={() => (showSettings = false)}
				/>
			{:else if preview}
				<ComparePreview
					pair={preview}
					duration={previewSource?.duration ?? 0}
					seeking={previewSeeking}
					onSeek={seekPreview}
					onClose={closePreview}
				/>
			{:else if detailJob}
				<ResultCard
					job={detailJob}
					{limitBytes}
					busy={previewBusy}
					onCopy={copy}
					onReveal={(path) => void revealInFolder(path)}
					onCompare={() => compare(detailJob)}
					onClear={() => {
						jobs.remove(detailJob.id);
						openJobId = null;
					}}
					onBack={canGoBack ? () => (openJobId = null) : null}
				/>
			{:else if jobs.jobs.length === 0}
				<DropZone {hovering} onBrowse={browse} />
			{:else}
				<div class="list">
					{#each jobs.jobs as job (job.id)}
						<FileRow
							{job}
							onCancel={(id) => void cancelJob(id)}
							onRemove={(id) => {
								jobs.remove(id);
								if (openJobId === id) openJobId = null;
							}}
							onSelect={(id) => (openJobId = id)}
							onCopy={copy}
						/>
					{/each}
				</div>
			{/if}
		</section>

		<footer>
			<span class="summary">
				{#if jobs.anyRunning}
					encoding · target {formatBytes(limitBytes)}
				{:else if jobs.finished.length > 0}
					{jobs.finished.length} done · target {formatBytes(limitBytes)}
				{:else}
					target {formatBytes(limitBytes)} · {options.codec}
				{/if}
				{#if replacing}
					· <span class="replacing">replacing originals</span>
				{/if}
			</span>

			{#if jobs.finished.length > 1}
				<button onclick={copyAllFinished}>Copy all</button>
			{/if}

			{#if jobs.anyRunning}
				<button onclick={() => void cancelAll()}>Stop</button>
			{:else if jobs.jobs.length > 0}
				<button onclick={() => jobs.clear()}>Clear</button>
			{/if}

			<button class="primary" onclick={browse}>Add files</button>
		</footer>
	{/if}

	{#if toast}
		<div class="toast">{toast}</div>
	{/if}
</main>

<style>
	main {
		position: relative;
		display: flex;
		flex-direction: column;
		margin: 8px;
		height: calc(100vh - 16px);
		background: var(--bg-panel);
		border: 1px solid var(--border);
		border-radius: var(--radius-lg);
		overflow: hidden;
	}

	header {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 10px 14px;
		border-bottom: 1px solid var(--border);
		flex: none;
	}

	.count {
		font-size: 12px;
		color: var(--text-secondary);
	}

	.target {
		margin-left: auto;
	}

	.icon {
		background: none;
		border: none;
		color: var(--text-muted);
		font-size: 14px;
		padding: 3px 6px;
		border-radius: 5px;
		line-height: 1;
	}

	.icon:hover,
	.icon.active {
		color: var(--text);
		background: var(--bg-row-hover);
	}

	.body {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 6px;
	}

	.body.empty {
		padding: 14px;
	}

	/* The comparison sizes itself to the pane, so the pane must stop scrolling
	   and hand over its height instead. */
	.body.fill {
		display: flex;
		overflow: hidden;
	}

	.list {
		display: flex;
		flex-direction: column;
	}

	footer {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 12px 14px;
		border-top: 1px solid var(--border);
		flex: none;
	}

	.summary {
		flex: 1;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	/* Loud enough to notice before dropping a file in, quiet enough not to
	   look like an error. */
	.replacing {
		color: var(--danger);
	}

	button {
		border: 1px solid var(--border-strong);
		background: none;
		color: var(--text-secondary);
		border-radius: 6px;
		padding: 5px 11px;
		font-size: 12px;
		flex: none;
	}

	button:hover {
		background: var(--bg-row-hover);
		color: var(--text);
	}

	button.primary {
		background: var(--accent);
		border-color: var(--accent);
		color: var(--accent-ink);
		font-weight: 500;
	}

	button.primary:hover {
		filter: brightness(1.1);
	}

	.toast {
		position: absolute;
		left: 50%;
		bottom: 62px;
		transform: translateX(-50%);
		background: #2c2c34;
		border: 1px solid var(--border-strong);
		border-radius: 6px;
		padding: 7px 14px;
		font-size: 12px;
		color: var(--text);
		box-shadow: 0 6px 20px rgba(0, 0, 0, 0.4);
		white-space: nowrap;
	}
</style>
