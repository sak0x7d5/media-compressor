<script lang="ts">
	import type { EncodeSettings, FfmpegStatus, PresetFile } from '$lib/ipc';
	import { setPresetsUrl, setShellMenu } from '$lib/ipc';

	let { settings, status, shellMenu, presetsSourceUrl, onChange, onShellMenu, onPresets, onClose }: {
		settings: EncodeSettings;
		status: FfmpegStatus | null;
		shellMenu: boolean;
		presetsSourceUrl: string;
		onChange: (patch: Partial<EncodeSettings>) => void;
		onShellMenu: (enabled: boolean) => void;
		onPresets: (presets: PresetFile) => void;
		onClose: () => void;
	} = $props();

	let shellBusy = $state(false);
	let shellError = $state<string | null>(null);

	/* Kept in sync with the prop rather than seeded from it once: saving writes
	   a new preset file back through the parent, and a field that captured only
	   the initial value would then disagree with what is stored. */
	let presetsUrl = $state('');
	$effect(() => {
		presetsUrl = presetsSourceUrl;
	});
	let urlError = $state<string | null>(null);

	async function saveUrl() {
		const next = presetsUrl.trim();
		if (next === presetsSourceUrl.trim()) return;

		urlError = null;
		try {
			onPresets(await setPresetsUrl(next.length > 0 ? next : null));
		} catch (error) {
			urlError = String(error);
		}
	}

	async function toggleShellMenu(enabled: boolean) {
		shellBusy = true;
		shellError = null;
		try {
			onShellMenu(await setShellMenu(enabled));
		} catch (error) {
			shellError = String(error);
		} finally {
			shellBusy = false;
		}
	}
</script>

<div class="panel">
	<div class="head">
		<span class="title">Settings</span>
		<button class="close" onclick={onClose} aria-label="Close settings">✕</button>
	</div>

	<label class="row">
		<span class="name">Video codec</span>
		<select
			value={settings.codec ?? 'h264'}
			onchange={(e) => onChange({ codec: e.currentTarget.value as EncodeSettings['codec'] })}
		>
			<option value="h264">H.264 — plays everywhere</option>
			<option value="hevc">H.265 — smaller, patchy support</option>
			<option value="av1">AV1 — smallest, no Discord preview</option>
		</select>
	</label>
	{#if settings.codec === 'av1'}
		<p class="warn">
			Discord cannot generate a thumbnail for AV1, so the file posts as a plain download link
			with no inline player. Fine for everywhere else.
		</p>
	{/if}

	<label class="row">
		<span class="name">Quality bias</span>
		<select
			value={settings.bias ?? 'balanced'}
			onchange={(e) => onChange({ bias: e.currentTarget.value as EncodeSettings['bias'] })}
		>
			<option value="resolution-first">Keep resolution, accept softness</option>
			<option value="balanced">Balanced</option>
			<option value="sharpness-first">Shrink more, stay crisp</option>
		</select>
	</label>

	<label class="row">
		<span class="name">Encoder effort</span>
		<select
			value={settings.speed ?? 'balanced'}
			onchange={(e) => onChange({ speed: e.currentTarget.value as EncodeSettings['speed'] })}
		>
			<option value="fast">Fast</option>
			<option value="balanced">Balanced</option>
			<option value="slow">Slow — best quality per byte</option>
		</select>
	</label>

	<label class="row">
		<span class="name">Image format</span>
		<select
			value={settings.image_format ?? 'webp'}
			onchange={(e) =>
				onChange({ image_format: e.currentTarget.value as EncodeSettings['image_format'] })}
		>
			<option value="webp">WebP — fast, great compression</option>
			<option value="avif">AVIF — smallest, much slower</option>
			<option value="jpeg">JPEG — maximum compatibility</option>
			<option value="png">PNG — lossless</option>
		</select>
	</label>

	<label class="row">
		<span class="name">Safety margin</span>
		<span class="control">
			<input
				type="range"
				min="80"
				max="100"
				step="1"
				value={Math.round((settings.safety_margin ?? 0.95) * 100)}
				oninput={(e) => onChange({ safety_margin: Number(e.currentTarget.value) / 100 })}
			/>
			<span class="value">{Math.round((settings.safety_margin ?? 0.95) * 100)}%</span>
		</span>
	</label>
	<p class="hint">
		Platforms disagree about whether "20 MB" means 20,000,000 or 20,971,520 bytes. Aiming a
		little under the line absorbs that.
	</p>

	<div class="row">
		<span class="name">Explorer right-click</span>
		<button class="toggle" class:on={shellMenu} disabled={shellBusy} onclick={() => toggleShellMenu(!shellMenu)}>
			{shellMenu ? 'Enabled' : 'Disabled'}
		</button>
	</div>
	{#if shellError}
		<p class="warn">{shellError}</p>
	{/if}

	<div class="row stack">
		<span class="name">Preset list URL</span>
		<input
			class="url"
			type="url"
			placeholder="https://… (leave empty for no network access)"
			bind:value={presetsUrl}
			onchange={saveUrl}
			onblur={saveUrl}
		/>
	</div>
	<p class="hint">
		Optional. Point this at a JSON file you control and the app re-checks it once a day, so a
		platform changing its cap doesn't need a new release. Empty — the default — means the app
		never fetches anything.
	</p>
	{#if urlError}
		<p class="warn">{urlError}</p>
	{/if}

	<div class="foot">
		<span class="mono">{status?.version ?? 'FFmpeg not installed'}</span>
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
		margin-bottom: 14px;
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

	.row {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 7px 0;
	}

	.name {
		flex: 1;
		font-size: 12px;
		color: var(--text-secondary);
	}

	select,
	.toggle {
		background: rgba(255, 255, 255, 0.04);
		border: 1px solid var(--border);
		color: var(--text-secondary);
		border-radius: 5px;
		padding: 4px 8px;
		font-size: 11px;
		font-family: var(--font-mono);
		min-width: 190px;
	}

	select:hover,
	.toggle:hover {
		border-color: var(--border-strong);
		color: var(--text);
	}

	select option {
		background: #23232a;
		color: var(--text);
	}

	.toggle.on {
		border-color: var(--accent);
		color: var(--accent);
	}

	.toggle:disabled {
		opacity: 0.6;
	}

	.row.stack {
		flex-direction: column;
		align-items: stretch;
		gap: 6px;
	}

	.url {
		background: rgba(255, 255, 255, 0.04);
		border: 1px solid var(--border);
		color: var(--text-secondary);
		border-radius: 5px;
		padding: 5px 8px;
		font-size: 11px;
		font-family: var(--font-mono);
		width: 100%;
	}

	.url:hover,
	.url:focus {
		border-color: var(--border-strong);
		color: var(--text);
		outline: none;
	}

	.control {
		display: flex;
		align-items: center;
		gap: 8px;
		min-width: 190px;
	}

	.control input {
		flex: 1;
		accent-color: var(--accent);
	}

	.value {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		min-width: 32px;
		text-align: right;
	}

	.hint,
	.warn {
		margin: 2px 0 8px;
		font-size: 11px;
		line-height: 1.5;
		color: var(--text-muted);
	}

	.warn {
		color: var(--warning);
	}

	.foot {
		margin-top: 12px;
		padding-top: 12px;
		border-top: 1px solid var(--border);
	}

	.mono {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--text-muted);
		word-break: break-all;
	}
</style>
