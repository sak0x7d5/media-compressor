<script lang="ts">
	import type { PreviewPair } from '$lib/ipc';

	let { pair, onClose }: { pair: PreviewPair; onClose: () => void } = $props();

	/* A wipe rather than side-by-side: compression artefacts are only visible
	   when the same pixels sit in the same place, and two panels put them
	   inches apart. */
	let split = $state(50);
	let dragging = $state(false);
	let frame: HTMLDivElement | undefined = $state();

	function setFromPointer(clientX: number) {
		if (!frame) return;
		const box = frame.getBoundingClientRect();
		if (box.width === 0) return;
		split = Math.max(0, Math.min(100, ((clientX - box.left) / box.width) * 100));
	}
</script>

<svelte:window
	onpointermove={(event) => dragging && setFromPointer(event.clientX)}
	onpointerup={() => (dragging = false)}
/>

<div class="wrap">
	<div class="head">
		<span class="label">Original</span>
		<span class="spacer"></span>
		<span class="label">Compressed</span>
		<button class="close" onclick={onClose} aria-label="Close comparison">✕</button>
	</div>

	<div
		class="frame"
		bind:this={frame}
		onpointerdown={(event) => {
			dragging = true;
			setFromPointer(event.clientX);
		}}
		role="presentation"
	>
		<img class="layer" src={pair.after} alt="Compressed frame" draggable="false" />
		<div class="layer clip" style:clip-path={`inset(0 ${100 - split}% 0 0)`}>
			<img class="layer" src={pair.before} alt="Original frame" draggable="false" />
		</div>

		<div class="handle" style:left={`${split}%`}>
			<div class="grip"></div>
		</div>
	</div>

	<!-- Keyboard equivalent for the drag handle. -->
	<input
		class="slider"
		type="range"
		min="0"
		max="100"
		step="1"
		bind:value={split}
		aria-label="Comparison position"
	/>
</div>

<style>
	.wrap {
		margin: 6px;
		background: var(--bg-row);
		border-radius: var(--radius);
		padding: 12px;
	}

	.head {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-bottom: 10px;
	}

	.label {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
	}

	.spacer {
		flex: 1;
	}

	.close {
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

	.frame {
		position: relative;
		width: 100%;
		border-radius: 6px;
		overflow: hidden;
		cursor: ew-resize;
		background: #000;
		line-height: 0;
		touch-action: none;
	}

	.layer {
		display: block;
		width: 100%;
		height: auto;
		user-select: none;
		-webkit-user-drag: none;
	}

	.clip {
		position: absolute;
		inset: 0;
	}

	.handle {
		position: absolute;
		top: 0;
		bottom: 0;
		width: 2px;
		background: rgba(255, 255, 255, 0.85);
		transform: translateX(-1px);
		pointer-events: none;
	}

	.grip {
		position: absolute;
		top: 50%;
		left: 50%;
		width: 22px;
		height: 22px;
		margin: -11px 0 0 -11px;
		border-radius: 50%;
		background: rgba(255, 255, 255, 0.9);
		box-shadow: 0 1px 6px rgba(0, 0, 0, 0.5);
	}

	.slider {
		width: 100%;
		margin-top: 10px;
		accent-color: var(--accent);
	}
</style>
