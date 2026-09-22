<script lang="ts">
	/* Shown once after an update lands, and on demand from Settings. The
	   entries come from the changelog compiled into the binary, so this works
	   with no network and cannot disagree with the version that is running. */

	import ChangeList from './ChangeList.svelte';
	import type { Release } from '$lib/ipc';

	let { title, subtitle, releases, dismissLabel, onClose }: {
		title: string;
		subtitle?: string;
		releases: Release[];
		dismissLabel: string;
		onClose: () => void;
	} = $props();
</script>

<div class="panel">
	<div class="head">
		<span class="title">{title}</span>
		<button class="close" onclick={onClose} aria-label="Close">✕</button>
	</div>

	{#if subtitle}
		<p class="subtitle">{subtitle}</p>
	{/if}

	{#each releases as release}
		<div class="release">
			<div class="version">
				<span class="number">{release.version}</span>
				{#if release.date}
					<span class="date">{release.date}</span>
				{/if}
			</div>
			<ChangeList sections={release.sections} />
		</div>
	{:else}
		<p class="subtitle">No release notes are recorded for this build.</p>
	{/each}

	<div class="foot">
		<button class="primary" onclick={onClose}>{dismissLabel}</button>
	</div>
</div>

<style>
	.panel {
		margin: 6px;
		background: var(--bg-row);
		border-radius: var(--radius);
		padding: 16px 18px;
	}

	.head {
		display: flex;
		align-items: center;
		margin-bottom: 4px;
	}

	.title {
		font-size: 13px;
		color: var(--text);
	}

	.close {
		margin-left: auto;
		background: none;
		border: none;
		color: var(--text-muted);
		padding: 2px 6px;
		border-radius: 4px;
	}

	.close:hover {
		color: var(--text);
		background: var(--bg-row-hover);
	}

	.subtitle {
		margin: 0 0 12px;
		font-size: 11px;
		line-height: 1.5;
		color: var(--text-muted);
	}

	.release + .release {
		margin-top: 18px;
		padding-top: 16px;
		border-top: 1px solid var(--border);
	}

	.version {
		display: flex;
		align-items: baseline;
		gap: 8px;
		margin-bottom: 8px;
	}

	.number {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text);
	}

	.date {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--text-muted);
	}

	.foot {
		margin-top: 16px;
		padding-top: 12px;
		border-top: 1px solid var(--border);
	}

	button.primary {
		background: var(--accent);
		border: 1px solid var(--accent);
		color: var(--accent-ink);
		border-radius: 6px;
		padding: 5px 13px;
		font-size: 12px;
		font-weight: 500;
	}

	button.primary:hover {
		filter: brightness(1.1);
	}
</style>
