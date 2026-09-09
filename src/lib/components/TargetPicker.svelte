<script lang="ts">
	import type { PresetFile } from '$lib/ipc';
	import { formatBytes } from '$lib/format';

	let { presets, selectedId, customBytes, onSelect, onCustom }: {
		presets: PresetFile | null;
		selectedId: string;
		customBytes: number;
		onSelect: (id: string) => void;
		onCustom: (bytes: number) => void;
	} = $props();

	const CUSTOM = '__custom__';

	const groups = $derived.by(() => {
		if (!presets) return [] as { name: string; items: PresetFile['presets'] }[];
		const out: { name: string; items: PresetFile['presets'] }[] = [];
		for (const preset of presets.presets) {
			const existing = out.find((group) => group.name === preset.group);
			if (existing) existing.items.push(preset);
			else out.push({ name: preset.group, items: [preset] });
		}
		return out;
	});

	const current = $derived(presets?.presets.find((preset) => preset.id === selectedId));

	/* Custom sizes are entered in MB because that is the unit every platform
	   quotes its limit in.

	   Kept in sync with the prop rather than seeded from it once: picking a
	   preset changes the byte count from outside this component, and a field
	   that captured only the initial value would then show a stale number. */
	let customMb = $state('');
	$effect(() => {
		// Shows the exact target rather than a rounded one. Rounding here made a
		// typed "8.5" snap back to "9" the moment it was committed, while the
		// app went on targeting 8.5 MB — the field and the target disagreed, and
		// the field was the one that was wrong.
		customMb = String(Number((customBytes / 1_000_000).toFixed(3)));
	});

	function commitCustom() {
		const parsed = Number.parseFloat(customMb);
		if (Number.isFinite(parsed) && parsed > 0) {
			onCustom(Math.round(parsed * 1_000_000));
		}
	}
</script>

<div class="picker">
	<select
		value={selectedId}
		onchange={(event) => onSelect((event.currentTarget as HTMLSelectElement).value)}
		aria-label="Target size"
	>
		{#each groups as group (group.name)}
			<optgroup label={group.name}>
				{#each group.items as preset (preset.id)}
					<option value={preset.id}>{preset.label} · {formatBytes(preset.bytes)}</option>
				{/each}
			</optgroup>
		{/each}
		<option value={CUSTOM}>Custom size…</option>
	</select>

	{#if selectedId === CUSTOM}
		<input
			class="custom"
			type="number"
			min="1"
			step="1"
			bind:value={customMb}
			onchange={commitCustom}
			onblur={commitCustom}
			aria-label="Custom size in megabytes"
		/>
		<span class="unit">MB</span>
	{/if}
</div>

{#if current?.note}
	<p class="note">{current.note}</p>
{/if}

<style>
	.picker {
		display: flex;
		align-items: center;
		gap: 6px;
	}

	select,
	input {
		background: rgba(255, 255, 255, 0.04);
		border: 1px solid var(--border);
		color: var(--text-secondary);
		border-radius: 5px;
		padding: 4px 8px;
		font-family: var(--font-mono);
		font-size: 11px;
	}

	select:hover,
	input:hover {
		border-color: var(--border-strong);
		color: var(--text);
	}

	/* The native dropdown paints its own list; force it dark so it does not
	   flash white against the rest of the window. */
	select option,
	select optgroup {
		background: #23232a;
		color: var(--text);
	}

	.custom {
		width: 84px;
		/* Right-aligned tabular digits so the number sits against the "MB" and
		   does not shift as it grows. */
		text-align: right;
		font-variant-numeric: tabular-nums;
		-moz-appearance: textfield;
		appearance: textfield;
	}

	/* WebView2 draws its own spinner arrows here, and at this size they render
	   as a cramped smudge that eats half the field. They are no loss: nobody
	   nudges an upload cap one megabyte at a time — this is a field you type
	   a number into. */
	.custom::-webkit-outer-spin-button,
	.custom::-webkit-inner-spin-button {
		-webkit-appearance: none;
		appearance: none;
		margin: 0;
	}

	.unit {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
	}

	.note {
		margin: 6px 0 0;
		font-size: 11px;
		color: var(--text-muted);
		line-height: 1.5;
	}
</style>
