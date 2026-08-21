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

	/* Each label hides once its side is nearly gone, so it never sits stranded
	   over the wrong image. */
	const showBefore = $derived(split > 12);
	const showAfter = $derived(split < 88);
</script>

<svelte:window
	onpointermove={(event) => dragging && setFromPointer(event.clientX)}
	onpointerup={() => (dragging = false)}
/>

<div class="wrap">
	<div class="head">
		<span class="title">Compare</span>
		<span class="hint">drag to wipe</span>
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

		<!-- Labels sit on the image rather than in the header. Against a busy
		     frame, muted text in a corner is invisible — these are opaque chips
		     with a hard border so they read over anything, and they are
		     pointer-transparent so they never swallow a drag. -->
		{#if showBefore}
			<span class="tag before">Original</span>
		{/if}
		{#if showAfter}
			<span class="tag after">Compressed</span>
		{/if}

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
		/* Fills the pane rather than growing past it. A comparison you have to
		   scroll cannot be judged: you never see the whole frame while wiping,
		   and a drag near the bottom edge fights the scrollbar for the same
		   gesture. Letterboxing costs some black bar; scrolling costs the
		   comparison itself. */
		display: flex;
		flex-direction: column;
		flex: 1;
		min-height: 0;
		margin: 6px;
		background: var(--bg-row);
		border-radius: var(--radius);
		padding: 12px;
	}

	.head {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-bottom: 10px;
		flex: none;
	}

	.title {
		font-size: 13px;
		color: var(--text);
	}

	.hint {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
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

	.frame {
		position: relative;
		width: 100%;
		flex: 1;
		min-height: 0;
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
		height: 100%;
		/* Both frames are extracted at the same width and aspect, so `contain`
		   lays them out identically — which the wipe depends on. */
		object-fit: contain;
		user-select: none;
		-webkit-user-drag: none;
	}

	.clip {
		position: absolute;
		inset: 0;
	}

	.tag {
		position: absolute;
		top: 10px;
		z-index: 2;
		pointer-events: none;
		background: rgba(10, 10, 12, 0.82);
		border: 1px solid rgba(255, 255, 255, 0.16);
		color: #ffffff;
		font-size: 11px;
		font-weight: 500;
		letter-spacing: 0.02em;
		line-height: 1;
		padding: 5px 9px;
		border-radius: 5px;
	}

	.tag.before {
		left: 10px;
	}

	.tag.after {
		right: 10px;
	}

	.handle {
		position: absolute;
		top: 0;
		bottom: 0;
		width: 2px;
		background: rgba(255, 255, 255, 0.85);
		transform: translateX(-1px);
		pointer-events: none;
		z-index: 3;
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
		flex: none;
		margin-top: 10px;
		accent-color: var(--accent);
	}
</style>
