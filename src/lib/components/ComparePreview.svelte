<script lang="ts">
	import { untrack } from 'svelte';

	import type { PreviewPair } from '$lib/ipc';
	import { formatDuration } from '$lib/format';

	let {
		pair,
		duration = 0,
		seeking = false,
		onSeek,
		onClose
	}: {
		pair: PreviewPair;
		/* Zero for a still, which has no timeline to move along. */
		duration?: number;
		/* A frame for a newer timestamp is still being extracted. */
		seeking?: boolean;
		/* `immediate` means the gesture is over: fetch now rather than
		   waiting to see whether more movement is coming. */
		onSeek: (seconds: number, immediate?: boolean) => void;
		onClose: () => void;
	} = $props();

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

	function nudge(by: number) {
		split = Math.max(0, Math.min(100, split + by));
	}

	/* The frame is the slider, so it takes the keys directly. A separate range
	   input below would be a second visible control for one value. */
	function onKeydown(event: KeyboardEvent) {
		const step = event.shiftKey ? 10 : 2;
		switch (event.key) {
			case 'ArrowLeft':
				nudge(-step);
				break;
			case 'ArrowRight':
				nudge(step);
				break;
			case 'Home':
				split = 0;
				break;
			case 'End':
				split = 100;
				break;
			default:
				return;
		}
		event.preventDefault();
	}

	/* Both halves of the wipe have to be the same moment, and two <img>
	   elements handed new sources in the same update do not paint in the same
	   update: they decode independently and each appears when it is ready. A
	   click hides this — one swap, then quiet — but dragging across frames
	   already in hand swaps continuously, and the seam sits there showing one
	   side a moment or two ahead of the other. A comparison that is not of the
	   same instant is worse than a slow one; it is quietly wrong.

	   So a pair is decoded off screen and only becomes visible once both sides
	   are ready, which is what makes the swap atomic. Until then the previous
	   pair stays up, still matched. */
	let shown = $state(untrack(() => pair));
	let generation = 0;

	function decoded(src: string): Promise<unknown> {
		const image = new Image();
		image.src = src;
		/* A source that will not decode is still shown: the alternative is a
		   comparison stuck on a stale frame with nothing to explain it. */
		return image.decode().catch(() => undefined);
	}

	$effect(() => {
		const next = pair;
		if (untrack(() => shown) === next) return;

		const token = ++generation;
		void Promise.all([decoded(next.before), decoded(next.after)]).then(() => {
			// A newer pair started decoding while this one was in flight.
			if (token === generation) shown = next;
		});
	});

	/* Still busy while the frames decode, not just while ffmpeg runs — what is
	   on screen is the old timestamp for both stretches. */
	const pending = $derived(seeking || shown !== pair);

	/* Each label hides once its side is nearly gone, so it never sits stranded
	   over the wrong image. */
	const showBefore = $derived(split > 12);
	const showAfter = $derived(split < 88);

	/* One frame is one sample of a decision that plays out over the whole clip:
	   the midpoint can be a locked-off shot while every artefact worth seeing
	   is in a pan ten seconds later. The timeline moves both frames together,
	   which is the only way to go looking for the damage rather than hoping it
	   landed under the default.

	   It stops just short of the end because seeking to exactly the duration
	   lands past the final frame and comes back empty. */
	const lastFrame = $derived(Math.max(0, duration - 0.1));
	const scrubbable = $derived(lastFrame > 0);

	/* The backend picks the midpoint when the comparison opens; from then on
	   this owns the position, so the knob stays under the pointer instead of
	   snapping back to whichever reply landed last. */
	let at = $state(untrack(() => pair.at_seconds));
	let scrubbing = $state(false);
	let track: HTMLDivElement | undefined = $state();

	const elapsed = $derived(scrubbable ? (at / lastFrame) * 100 : 0);

	function seekTo(seconds: number) {
		/* Rounded to tenths so that dragging back over ground already covered
		   asks for timestamps that have been fetched before, instead of
		   near-misses that no cache can answer. Two frames 100ms apart are the
		   same shot anyway. */
		const clamped = Math.max(0, Math.min(lastFrame, seconds));
		const next = Math.round(clamped * 10) / 10;
		if (next === at) return;
		at = next;
		onSeek(next);
	}

	function seekFromPointer(clientX: number) {
		if (!track) return;
		const box = track.getBoundingClientRect();
		if (box.width === 0) return;
		seekTo(((clientX - box.left) / box.width) * lastFrame);
	}

	/* Whole seconds, because the neighbouring frame is the same picture: the
	   point of moving at all is to reach a different shot. */
	function onTimeKeydown(event: KeyboardEvent) {
		const step = event.shiftKey ? 10 : 1;
		switch (event.key) {
			case 'ArrowLeft':
				seekTo(at - step);
				break;
			case 'ArrowRight':
				seekTo(at + step);
				break;
			case 'Home':
				seekTo(0);
				break;
			case 'End':
				seekTo(lastFrame);
				break;
			default:
				return;
		}
		event.preventDefault();
	}
</script>

<svelte:window
	onpointermove={(event) => {
		if (dragging) setFromPointer(event.clientX);
		if (scrubbing) seekFromPointer(event.clientX);
	}}
	onpointerup={() => {
		dragging = false;
		if (scrubbing) {
			scrubbing = false;
			/* The drag is finished, so there is nothing left to coalesce. */
			onSeek(at, true);
		}
	}}
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
		onkeydown={onKeydown}
		role="slider"
		tabindex="0"
		aria-label="Comparison position"
		aria-orientation="horizontal"
		aria-valuemin={0}
		aria-valuemax={100}
		aria-valuenow={Math.round(split)}
		aria-valuetext={`${Math.round(split)}% original`}
	>
		<img class="layer" src={shown.after} alt="Compressed frame" draggable="false" />
		<div class="layer clip" style:clip-path={`inset(0 ${100 - split}% 0 0)`}>
			<img class="layer" src={shown.before} alt="Original frame" draggable="false" />
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

	{#if scrubbable}
		<div class="timeline">
			<span class="time" class:pending>{formatDuration(at)}</span>
			<div
				class="track"
				bind:this={track}
				onpointerdown={(event) => {
					scrubbing = true;
					seekFromPointer(event.clientX);
				}}
				onkeydown={onTimeKeydown}
				role="slider"
				tabindex="0"
				aria-label="Preview time"
				aria-orientation="horizontal"
				aria-valuemin={0}
				aria-valuemax={Math.round(lastFrame)}
				aria-valuenow={Math.round(at)}
				aria-valuetext={`${formatDuration(at)} of ${formatDuration(duration)}`}
			>
				<div class="elapsed" style:width={`${elapsed}%`}></div>
				<div class="knob" style:left={`${elapsed}%`}></div>
			</div>
			<span class="total">{formatDuration(duration)}</span>
		</div>
	{/if}
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

	/* Only on keyboard focus: a ring around the image every time it is clicked
	   would be noise. */
	.frame:focus-visible {
		outline: 2px solid var(--accent);
		outline-offset: 2px;
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

	.timeline {
		display: flex;
		align-items: center;
		gap: 10px;
		flex: none;
		margin-top: 12px;
	}

	.time,
	.total {
		font-family: var(--font-mono);
		font-size: 11px;
		line-height: 1;
		flex: none;
	}

	.time {
		color: var(--text-secondary);
	}

	/* Muted while the next frame is still being extracted. What is on screen is
	   the previous timestamp until it lands, and saying so costs a colour
	   rather than a spinner that would blink on every step. */
	.time.pending {
		color: var(--text-muted);
	}

	.total {
		color: var(--text-muted);
	}

	.track {
		position: relative;
		flex: 1;
		height: 4px;
		border-radius: 2px;
		background: var(--bg-track);
		cursor: pointer;
		touch-action: none;
	}

	.track:focus-visible {
		outline: 2px solid var(--accent);
		outline-offset: 5px;
	}

	/* Four pixels of bar is not a drag target. The hit area is the height of a
	   row; the bar itself stays thin. */
	.track::before {
		content: '';
		position: absolute;
		inset: -10px 0;
	}

	.elapsed {
		position: absolute;
		top: 0;
		bottom: 0;
		left: 0;
		border-radius: 2px;
		background: var(--accent);
	}

	.knob {
		position: absolute;
		top: 50%;
		width: 11px;
		height: 11px;
		margin: -5.5px 0 0 -5.5px;
		border-radius: 50%;
		background: rgba(255, 255, 255, 0.92);
		box-shadow: 0 1px 4px rgba(0, 0, 0, 0.5);
		pointer-events: none;
	}
</style>
