<script lang="ts">
	import type { InstallProgress, SystemBuild } from '$lib/ipc';
	import { formatBytes } from '$lib/format';

	let { progress, error, busy, system, shellSupported, onInstall, onUseSystem }: {
		progress: InstallProgress | null;
		error: string | null;
		busy: boolean;
		/** What's on PATH, if anything. Null once nothing was found there. */
		system: SystemBuild | null;
		shellSupported: boolean;
		/* Both ways of getting an FFmpeg take the checkbox, because the entry is
		   only worth adding once there is something for it to open. */
		onInstall: (addToExplorerMenu: boolean) => void;
		onUseSystem: (addToExplorerMenu: boolean) => void;
	} = $props();

	/* Ticked by default. The entry is the whole point of installing this, it
	   writes only under the current user, and Settings can undo it — so the
	   cost of it being on and unwanted is one toggle, while the cost of it
	   being off is nobody ever finding out the feature exists. */
	let addToExplorerMenu = $state(true);

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

	<!--
		A usable build here means startup stopped waiting on it rather than that
		it was turned down — see PROBE_TIMEOUT. Offering it beats making someone
		download eighty megabytes they demonstrably do not need.
	-->
	{#if system?.usable}
		<p class="found">
			You already have FFmpeg at <span class="mono">{system.location}</span>, and it can do
			everything this app needs.
		</p>
	{/if}

	<!--
		Someone who knows they already have FFmpeg deserves to be told why it
		isn't being used, rather than left to conclude the app never looked.
	-->
	{#if system && !system.usable}
		<p class="found">
			You do have FFmpeg at <span class="mono">{system.location}</span>, but it was built
			without {system.missing_encoders.join(' and ')} — which this app needs. The download
			below is a build that has everything.
		</p>
	{/if}

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
		{#if shellSupported}
			<label class="opt">
				<input type="checkbox" bind:checked={addToExplorerMenu} />
				<span>
					Add <strong>Compress for Discord</strong> to the right-click menu for videos and
					images. On Windows&nbsp;11 it appears under “Show more options”.
				</span>
			</label>
		{/if}

		<div class="actions">
			{#if system?.usable}
				<button class="primary" onclick={() => onUseSystem(addToExplorerMenu)}>Use the one I have</button>
				<button class="secondary" onclick={() => onInstall(addToExplorerMenu)}>Download a copy anyway</button>
			{:else}
				<button class="primary" onclick={() => onInstall(addToExplorerMenu)}>
					{error ? 'Try again' : 'Download FFmpeg'}
				</button>
			{/if}
		</div>
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

	.found {
		font-size: 12px;
		color: var(--text-muted);
	}

	.mono {
		font-family: var(--font-mono);
		word-break: break-all;
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

	.opt {
		display: flex;
		align-items: flex-start;
		gap: 9px;
		margin: 0 0 18px;
		font-size: 13px;
		line-height: 1.55;
		color: var(--text-secondary);
		cursor: pointer;
	}

	.opt input {
		/* The native checkbox sits on the text baseline rather than the cap
		   height, so it needs nudging down to line up with the first line. */
		margin: 2px 0 0;
		accent-color: var(--accent);
		flex: none;
	}

	.opt strong {
		color: var(--text);
		font-weight: 500;
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

	.actions {
		display: flex;
		gap: 8px;
		align-items: center;
	}

	button.secondary {
		background: none;
		border: 1px solid var(--border);
		color: var(--text-secondary);
		border-radius: 6px;
		padding: 7px 15px;
		font-size: 13px;
	}

	button.secondary:hover {
		color: var(--text);
		background: var(--bg-row-hover);
	}
</style>
