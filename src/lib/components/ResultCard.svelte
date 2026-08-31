<script lang="ts">
	import type { Job } from '$lib/jobs.svelte';
	import { formatBytes, formatReduction } from '$lib/format';

	let { job, limitBytes, busy, onCopy, onReveal, onCompare, onClear, onBack = null }: {
		job: Job;
		limitBytes: number;
		busy: boolean;
		onCopy: (path: string) => void;
		onReveal: (path: string) => void;
		onCompare: () => void;
		onClear: () => void;
		/* Present only when this card was opened from a queue of several files,
		   in which case dismissing it should return to the list rather than
		   discard the job. */
		onBack?: (() => void) | null;
	} = $props();

	const outBytes = $derived(job.outcome?.output_bytes ?? 0);
	const reduction = $derived(formatReduction(job.inputBytes, outBytes));

	/* How much of the allowance was used. Landing just under the line is the
	   goal — unused budget is quality that was thrown away, so a very short bar
	   is a worse outcome than a nearly-full one, not a better one. */
	const usedFraction = $derived(limitBytes > 0 ? Math.min(outBytes / limitBytes, 1) : 0);
	const overLimit = $derived(job.outcome ? !job.outcome.within_limit : false);
	const barColor = $derived(overLimit ? 'var(--danger)' : 'var(--success)');

	/* Video counts re-encodes; an image counts quality probes. Both are "how
	   many tries did this take", which is what the failure message needs. */
	const attempts = $derived(
		job.outcome?.kind === 'video'
			? job.outcome.attempts
			: job.outcome?.kind === 'image'
				? job.outcome.encodes
				: 0
	);

	/* The comparison reads both files. Once a source has been replaced there is
	   no "before" left to read, so the button goes rather than offering a click
	   that can only fail. */
	const comparable = $derived(job.original !== 'replaced');
</script>

<div class="card">
	<div class="head">
		{#if onBack}
			<button class="back" onclick={onBack} title="Back to the queue" aria-label="Back to the queue">‹</button>
		{:else}
			<span class="tick" style:color={barColor} aria-hidden="true">{overLimit ? '!' : '✓'}</span>
		{/if}
		<span class="name" title={job.output}>{job.name}</span>
		<span class="spec">{job.detail}</span>
	</div>

	<div class="sizes">
		<span class="before">{formatBytes(job.inputBytes)}</span>
		<span class="arrow" aria-hidden="true">→</span>
		<span class="after">{formatBytes(outBytes)}</span>
		{#if reduction}
			<span class="reduction" style:color={barColor}>{reduction}</span>
		{/if}
	</div>

	<div class="track">
		<div class="fill" style:width={`${Math.round(usedFraction * 100)}%`} style:background={barColor}></div>
	</div>
	<div class="scale">
		<span>0</span>
		<span>{formatBytes(limitBytes)} limit</span>
	</div>

	{#if overLimit}
		<p class="warning">
			Could not get under the limit after {attempts} attempts. The file is still here, but it
			will not upload.
		</p>
	{/if}

	{#if job.original === 'replaced'}
		<p class="note">The original was replaced by this file.</p>
	{:else if job.original === 'kept-not-smaller'}
		<p class="warning">
			The compressed version came out no smaller than the original, so it was discarded and
			your file was left as it was.
		</p>
	{/if}

	<div class="actions">
		<button class="primary" onclick={() => onCopy(job.output)}>Copy to clipboard</button>
		<button onclick={() => onReveal(job.output)}>Show in folder</button>
		{#if comparable}
			<button onclick={onCompare} disabled={busy}>{busy ? 'Loading…' : 'Compare'}</button>
		{/if}
		<button class="trailing" onclick={onBack ?? onClear}>{onBack ? 'Back to queue' : 'Done'}</button>
	</div>
</div>

<style>
	.card {
		background: var(--bg-row);
		border-radius: var(--radius);
		padding: 20px 18px;
		margin: 6px;
	}

	.head {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-bottom: 16px;
		min-width: 0;
	}

	.tick {
		font-size: 14px;
		flex: none;
	}

	.back {
		flex: none;
		background: none;
		border: none;
		color: var(--text-muted);
		font-size: 18px;
		line-height: 1;
		padding: 0 4px;
		border-radius: 4px;
	}

	.back:hover {
		color: var(--text);
		background: var(--bg-row-hover);
	}

	.name {
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.spec {
		margin-left: auto;
		padding-left: 12px;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		white-space: nowrap;
		flex: none;
	}

	.sizes {
		display: flex;
		align-items: baseline;
		gap: 12px;
		margin-bottom: 14px;
		font-family: var(--font-mono);
		font-variant-numeric: tabular-nums;
	}

	.before {
		font-size: 15px;
		color: var(--text-muted);
		text-decoration: line-through;
	}

	.arrow {
		color: var(--text-muted);
	}

	.after {
		font-size: 30px;
		font-weight: 500;
		color: var(--text);
	}

	.reduction {
		margin-left: auto;
		font-size: 12px;
	}

	.track {
		height: 6px;
		background: var(--bg-track);
		border-radius: 3px;
		overflow: hidden;
		margin-bottom: 6px;
	}

	.fill {
		height: 100%;
		border-radius: 3px;
		transition: width 320ms ease;
	}

	.scale {
		display: flex;
		justify-content: space-between;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		margin-bottom: 18px;
	}

	.warning {
		margin: 0 0 16px;
		font-size: 12px;
		line-height: 1.5;
		color: var(--danger);
	}

	/* Stating what happened, not warning about it — a replacement that went as
	   asked is the normal outcome of that mode. */
	.note {
		margin: 0 0 16px;
		font-size: 12px;
		line-height: 1.5;
		color: var(--text-muted);
	}

	.actions {
		display: flex;
		gap: 8px;
	}

	button {
		border: 1px solid var(--border-strong);
		background: none;
		color: var(--text-secondary);
		border-radius: 6px;
		padding: 6px 13px;
		font-size: 12px;
	}

	button:hover {
		background: var(--bg-row-hover);
		color: var(--text);
	}

	button:disabled {
		opacity: 0.55;
		cursor: default;
	}

	button:disabled:hover {
		background: none;
		color: var(--text-secondary);
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

	button.trailing {
		margin-left: auto;
	}
</style>
