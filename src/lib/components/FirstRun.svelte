<script lang="ts">
	import type { InstallProgress } from '$lib/ipc';
	import { formatBytes } from '$lib/format';

	let { progress, error, busy, onInstall }: {
		progress: InstallProgress | null;
		error: string | null;
		busy: boolean;
		onInstall: () => void;
	} = $props();

	const fraction = $derived(
		progress?.step === 'downloading' && progress.total_bytes > 0
			? progress.downloaded_bytes / progress.total_bytes
			: null
	);

	const label = $derived.by(() => {
		if (!progress) return '';
		switch (progress.step) {
			case 'starting':
				return 'Contacting the download server…';
			case 'downloading':
				return `Downloading ${formatBytes(progress.downloaded_bytes)} of ${formatBytes(progress.total_bytes)}`;
			case 'unpacking':
				return 'Unpacking…';
			case 'verifying':
				return 'Checking the binaries run…';
			case 'done':
				return 'Ready.';
		}
	});
</script>

<div class="first-run">
	<h1>One thing first</h1>
	<p>
		This app compresses with FFmpeg, which isn't bundled in the installer. It's a one-time
		download of about 80&nbsp;MB, kept in this app's own folder — nothing else on your system is
		touched.
	</p>

	{#if error}
		<p class="error">{error}</p>
	{/if}

	{#if busy}
		<div class="track">
			{#if fraction === null}
				<div class="fill indeterminate"></div>
			{:else}
				<div class="fill" style:width={`${Math.round(fraction * 100)}%`}></div>
			{/if}
		</div>
		<p class="status">{label}</p>
	{:else}
		<button class="primary" onclick={onInstall}>
			{error ? 'Try again' : 'Download FFmpeg'}
		</button>
	{/if}
</div>

<style>
	.first-run {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		justify-content: center;
		height: 100%;
		padding: 32px 34px;
		max-width: 460px;
	}

	h1 {
		margin: 0 0 10px;
		font-size: 18px;
		font-weight: 500;
		color: var(--text);
	}

	p {
		margin: 0 0 18px;
		font-size: 13px;
		line-height: 1.6;
		color: var(--text-secondary);
	}

	.error {
		color: var(--danger);
	}

	.track {
		width: 100%;
		height: 4px;
		background: var(--bg-track);
		border-radius: 2px;
		overflow: hidden;
		margin-bottom: 10px;
	}

	.fill {
		height: 100%;
		background: var(--accent);
		border-radius: 2px;
		transition: width 200ms ease;
	}

	.fill.indeterminate {
		width: 35%;
		animation: slide 1.4s ease-in-out infinite;
	}

	@keyframes slide {
		0% {
			transform: translateX(-100%);
		}
		100% {
			transform: translateX(340%);
		}
	}

	.status {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		margin: 0;
	}

	button.primary {
		background: var(--accent);
		border: 1px solid var(--accent);
		color: var(--accent-ink);
		border-radius: 6px;
		padding: 7px 15px;
		font-size: 13px;
		font-weight: 500;
	}

	button.primary:hover {
		filter: brightness(1.1);
	}
</style>
