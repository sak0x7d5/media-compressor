<script lang="ts">
	/* One renderer for both sources of release notes: the entries Rust parsed
	   out of the compiled-in changelog, and the free text attached to an update
	   manifest. Inline markup is built from elements rather than injected as
	   HTML — see the note in $lib/changelog. */

	import { spans } from '$lib/changelog';
	import type { ChangeSection } from '$lib/ipc';

	let { sections }: { sections: ChangeSection[] } = $props();
</script>

{#each sections as section}
	<div class="group">
		<span class="heading">{section.heading}</span>
		<ul>
			{#each section.items as item}
				<li>{#each spans(item) as span}{#if span.kind === 'strong'}<strong>{span.text}</strong>{:else if span.kind === 'code'}<code>{span.text}</code>{:else}{span.text}{/if}{/each}</li>
			{/each}
		</ul>
	</div>
{/each}

<style>
	.group + .group {
		margin-top: 10px;
	}

	.heading {
		display: block;
		font-size: 10px;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--text-muted);
		margin-bottom: 4px;
	}

	ul {
		margin: 0;
		padding-left: 16px;
		list-style: none;
	}

	li {
		position: relative;
		font-size: 12px;
		line-height: 1.55;
		color: var(--text-secondary);
		padding: 1px 0;
	}

	li::before {
		content: '·';
		position: absolute;
		left: -12px;
		color: var(--text-muted);
	}

	strong {
		font-weight: 500;
		color: var(--text);
	}

	code {
		font-family: var(--font-mono);
		font-size: 11px;
		background: rgba(255, 255, 255, 0.06);
		border-radius: 3px;
		padding: 0 4px;
	}
</style>
