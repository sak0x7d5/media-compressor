<script lang="ts">
	let { hovering, onBrowse }: { hovering: boolean; onBrowse: () => void } = $props();
</script>

<button class="zone" class:hovering onclick={onBrowse}>
	<span class="glyph" aria-hidden="true">↓</span>
	<span class="headline">Drop video or images here</span>
	<span class="hint">or click to browse</span>
</button>

<style>
	.zone {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 6px;
		width: 100%;
		height: 100%;
		min-height: 200px;
		background: none;
		border: 1px dashed var(--border-strong);
		border-radius: var(--radius-lg);
		color: var(--text-muted);
		transition:
			border-color 140ms ease,
			background 140ms ease;
	}

	.zone:hover,
	.zone.hovering {
		border-color: var(--accent);
		background: rgba(127, 119, 221, 0.07);
	}

	.glyph {
		font-size: 22px;
		line-height: 1;
		margin-bottom: 4px;
	}

	.headline {
		color: var(--text-secondary);
		font-size: 13px;
	}

	.hint {
		font-size: 11px;
	}
</style>
