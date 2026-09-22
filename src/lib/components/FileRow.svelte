<script lang="ts">
	import type { Job } from '$lib/jobs.svelte';
	import { formatPercent } from '$lib/format';

	let { job, paused = false, onCancel, onRemove, onSelect, onCopy }: {
		job: Job;
		/** Whether the queue as a whole is stopped. */
		paused?: boolean;
		onCancel: (id: string) => void;
		onRemove: (id: string) => void;
		onSelect: (id: string) => void;
		onCopy: (path: string) => void;
	} = $props();

	const running = $derived(job.state === 'running');
	const settled = $derived(job.state === 'done' || job.state === 'failed' || job.state === 'cancelled');
	/* Only a finished job has a result worth opening. */
	const selectable = $derived(job.state === 'done');

	/* A stopped queue must not leave rows saying "queued", which reads as "your
	   turn is coming" when nothing is going to happen until Resume. */
	const label = $derived(
		job.state === 'done' ? '✓' : paused && job.state === 'queued' ? 'paused' : job.status
	);

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

<!-- The row is a real button when the job has finished, but the role is set
     conditionally so an unfinished row does not announce itself as clickable —
     which the static check cannot see through. It cannot be a <button> element
     either, because it contains one (the dismiss control). -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
	class="row"
	class:running
	class:selectable
	role={selectable ? 'button' : undefined}
	tabindex={selectable ? 0 : undefined}
	title={selectable ? 'Open result' : job.input}
	onclick={() => selectable && onSelect(job.id)}
	onkeydown={(event) => {
		if (selectable && (event.key === 'Enter' || event.key === ' ')) {
			event.preventDefault();
			onSelect(job.id);
		}
	}}
>
	<div class="labels">
		<div class="name">
			{job.name}
			<!-- Queued under a mode the settings may since have changed, so the
			     footer's warning cannot speak for this row. -->
			{#if job.replacesInput}
				<span class="replaces" title="The original will be replaced by the result">replaces</span>
			{/if}
		</div>
		<div class="detail">{job.detail}</div>
	</div>

	<!-- Once the size line reads "312 MB -> 19.2 MB", the word "done" adds
	     nothing, so a finished row gets a tick and the space goes to the
	     actions. Every other state still needs its word. -->
	<div class="status" style:color={statusColor}>{label}</div>

	<div class="trailing">
		{#if running}
			<span class="percent">{formatPercent(job.fraction)}</span>
		{/if}

		<!-- Every control here stops propagation: the row itself is clickable,
		     and a button that also opened the detail view would be maddening. -->
		{#if selectable}
			<button
				class="action primary"
				onclick={(event) => {
					event.stopPropagation();
					onCopy(job.output);
				}}
				title="Copy the result, ready to paste"
			>
				Copy
			</button>
			<button
				class="action"
				onclick={(event) => {
					event.stopPropagation();
					onSelect(job.id);
				}}
				title="Open result"
				aria-label="Open result"
			>
				⋯
			</button>
		{/if}

		<button
			class="ghost"
			onclick={(event) => {
				event.stopPropagation();
				// Dismissing means "get this out of my list". Something still
				// active has to be stopped too, but the row goes either way —
				// cancelling without removing left the ✕ looking broken.
				if (!settled) onCancel(job.id);
				onRemove(job.id);
			}}
			title={running ? 'Stop and remove' : 'Remove from list'}
		>
			✕
		</button>
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

	.row.selectable {
		cursor: pointer;
	}

	.row.selectable:focus-visible {
		outline: 1px solid var(--accent);
		outline-offset: -1px;
	}

	.action {
		border: 1px solid var(--border-strong);
		background: none;
		color: var(--text-secondary);
		border-radius: 5px;
		padding: 3px 9px;
		font-size: 11px;
		line-height: 1.5;
	}

	.action:hover {
		background: var(--bg-row-hover);
		color: var(--text);
	}

	/* Copy fills only when the pointer is on Copy itself — never on hover of the
	   row. Lighting it up while the pointer sits on the dismiss control says
	   "clicking does this" about an action that is not the one under the
	   cursor. */
	.action.primary:hover,
	.action.primary:focus-visible {
		background: var(--accent);
		border-color: var(--accent);
		color: var(--accent-ink);
		font-weight: 500;
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

	/* A quiet tag rather than a warning: the mode was chosen deliberately, and
	   a row per file shouting about it would be noise. */
	.replaces {
		margin-left: 6px;
		padding: 1px 5px;
		border-radius: 4px;
		background: var(--bg-row-hover);
		color: var(--text-muted);
		font-family: var(--font-mono);
		font-size: 10px;
		vertical-align: 1px;
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
		flex: none;
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
