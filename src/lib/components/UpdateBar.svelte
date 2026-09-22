<script lang="ts">
	/* The one piece of chrome that appears without being asked for, so it stays
	   one line tall until someone wants more from it. */

	import ChangeList from './ChangeList.svelte';
	import { formatBytes } from '$lib/format';
	import type { Updater } from '$lib/updates.svelte';

	let { updater, blocked, onDismiss }: {
		updater: Updater;
		/** Encoding right now. Installing would kill the job mid-file. */
		blocked: boolean;
		onDismiss: () => void;
	} = $props();

	let expanded = $state(false);

	const label = $derived.by(() => {
		switch (updater.phase) {
			case 'downloading':
				return updater.totalBytes
					? `Downloading ${formatBytes(updater.receivedBytes)} of ${formatBytes(updater.totalBytes)}`
					: `Downloading ${formatBytes(updater.receivedBytes)}`;
			case 'installing':
				return 'Installing — the app will restart itself';
			case 'error':
				return updater.message ?? 'The update failed';
			default:
				return `Version ${updater.version} is available`;
		}
	});
</script>

<div class="bar" class:failed={updater.phase === 'error'}>
	<div class="line">
		<span class="dot"></span>
		<span class="label">{label}</span>

		{#if updater.phase === 'available'}
			{#if updater.notes.length > 0}
				<button class="link" onclick={() => (expanded = !expanded)}>
					{expanded ? 'Hide' : "What's changed"}
				</button>
			{/if}
			<button
				class="go"
				disabled={blocked}
				title={blocked ? 'Finish the queue first — installing closes the app' : undefined}
				onclick={() => void updater.install()}
			>
				Update
			</button>
			<button class="close" onclick={onDismiss} aria-label="Dismiss">✕</button>
		{:else if updater.phase === 'error'}
			<button class="go" onclick={() => void updater.retry()}>Try again</button>
			<button class="close" onclick={onDismiss} aria-label="Dismiss">✕</button>
		{/if}
	</div>

	{#if updater.busy}
		<div class="track">
			{#if updater.fraction === null}
				<div class="fill indeterminate"></div>
			{:else}
				<div class="fill" style:width={`${Math.round(updater.fraction * 100)}%`}></div>
			{/if}
		</div>
	{/if}

	{#if blocked && updater.phase === 'available'}
		<p class="note">Installing closes the app, so this waits until the queue is empty.</p>
	{/if}

	{#if expanded && updater.phase === 'available'}
		<div class="notes">
			<ChangeList sections={updater.notes} />
		</div>
	{/if}
</div>

<style>
	.bar {
		flex: none;
		padding: 8px 14px;
		border-bottom: 1px solid var(--border);
		background: rgba(127, 119, 221, 0.1);
	}

	.bar.failed {
		background: rgba(226, 99, 90, 0.1);
	}

	.line {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--accent);
		flex: none;
	}

	.failed .dot {
		background: var(--danger);
	}

	.label {
		flex: 1;
		font-size: 12px;
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	button {
		flex: none;
		border-radius: 5px;
		font-size: 11px;
		padding: 3px 9px;
	}

	.link {
		background: none;
		border: none;
		color: var(--text-secondary);
		text-decoration: underline;
		text-underline-offset: 2px;
		padding: 3px 2px;
	}

	.link:hover {
		color: var(--text);
	}

	.go {
		background: var(--accent);
		border: 1px solid var(--accent);
		color: var(--accent-ink);
		font-weight: 500;
	}

	.go:hover:not(:disabled) {
		filter: brightness(1.1);
	}

	.go:disabled {
		background: none;
		border-color: var(--border-strong);
		color: var(--text-muted);
		cursor: default;
	}

	.close {
		background: none;
		border: none;
		color: var(--text-muted);
		padding: 2px 4px;
		line-height: 1;
	}

	.close:hover {
		color: var(--text);
	}

	.track {
		height: 3px;
		background: var(--bg-track);
		border-radius: 2px;
		overflow: hidden;
		margin-top: 8px;
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

	.note {
		margin: 6px 0 0;
		font-size: 11px;
		color: var(--text-muted);
	}

	.notes {
		margin-top: 10px;
		max-height: 168px;
		overflow-y: auto;
		padding-right: 4px;
	}
</style>
