<script lang="ts">
	import type { Job } from '$lib/jobs.svelte';
	import { formatPercent } from '$lib/format';

	let { job, onCancel, onRemove }: {
		job: Job;
		onCancel: (id: string) => void;
		onRemove: (id: string) => void;
	} = $props();

	const running = $derived(job.state === 'running');
	const settled = $derived(job.state === 'done' || job.state === 'failed' || job.state === 'cancelled');

	const statusColor = $derived(
		job.state === 'done'
			? 'var(--success)'
			: job.state === 'failed'
				? 'var(--danger)'
				: job.state === 'cancelled'
					? 'var(--text-muted)'
					: running
						? 'var(--accent)'
						: 'var(--text-muted)'
	);
</script>

<div class="row" class:running>
	<div class="labels">
		<div class="name" title={job.input}>{job.name}</div>
		<div class="detail">{job.detail}</div>
	</div>

	<div class="status" style:color={statusColor}>{job.status}</div>

	<div class="trailing">
		{#if running}
			<span class="percent">{formatPercent(job.fraction)}</span>
			<button class="ghost" onclick={() => onCancel(job.id)} title="Cancel">✕</button>
		{:else if settled}
			<button class="ghost" onclick={() => onRemove(job.id)} title="Remove from list">✕</button>
		{:else}
			<button class="ghost" onclick={() => onCancel(job.id)} title="Remove from queue">✕</button>
		{/if}
	</div>
</div>

<!-- The row *is* the progress bar. A separate widget would only repeat it. -->
<div class="track" aria-hidden="true">
	<div class="fill" style:width={`${Math.round(job.fraction * 100)}%`} style:background={statusColor}></div>
</div>

<style>
	.row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto auto;
		gap: 12px;
		align-items: center;
		padding: 11px 12px;
		border-radius: var(--radius);
	}

	.row.running {
		background: var(--bg-row);
	}

	.row:not(.running):hover {
		background: var(--bg-row-hover);
	}

	.labels {
		min-width: 0;
	}

	.name {
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.detail {
		margin-top: 2px;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.status {
		font-family: var(--font-mono);
		font-size: 12px;
		text-align: right;
	}

	.trailing {
		display: flex;
		align-items: center;
		gap: 6px;
		min-width: 62px;
		justify-content: flex-end;
	}

	.percent {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-secondary);
		/* Fixed width so the row does not jitter as the number grows. */
		font-variant-numeric: tabular-nums;
		min-width: 34px;
		text-align: right;
	}

	.ghost {
		background: none;
		border: none;
		padding: 2px 4px;
		border-radius: 4px;
		color: var(--text-muted);
		opacity: 0;
		transition: opacity 120ms ease;
	}

	.row:hover .ghost,
	.ghost:focus-visible {
		opacity: 1;
	}

	.ghost:hover {
		color: var(--text);
		background: var(--bg-row-hover);
	}

	.track {
		height: 2px;
		margin: 0 12px;
		background: var(--bg-track);
		overflow: hidden;
	}

	.fill {
		height: 100%;
		transition: width 200ms ease;
	}
</style>
