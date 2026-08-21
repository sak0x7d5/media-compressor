/** Display helpers. Every number the UI shows goes through one of these. */

/**
 * Sizes are rendered in decimal MB because that is the unit every upload limit
 * is quoted in. Showing MiB next to a "20 MB" target would invite exactly the
 * confusion the safety margin exists to absorb.
 */
export function formatBytes(bytes: number): string {
	if (!Number.isFinite(bytes) || bytes < 0) return '—';
	if (bytes < 1000) return `${bytes} B`;
	if (bytes < 1000 * 1000) return `${(bytes / 1000).toFixed(0)} KB`;

	const mb = bytes / (1000 * 1000);
	if (mb < 10) return `${mb.toFixed(2)} MB`;
	if (mb < 1000) return `${mb.toFixed(1)} MB`;
	return `${(mb / 1000).toFixed(2)} GB`;
}

export function formatBitrate(bps: number): string {
	if (!Number.isFinite(bps) || bps <= 0) return '—';
	if (bps < 1000 * 1000) return `${Math.round(bps / 1000)} kbps`;
	return `${(bps / 1000 / 1000).toFixed(2)} Mbps`;
}

export function formatDuration(seconds: number): string {
	if (!Number.isFinite(seconds) || seconds < 0) return '—';
	const total = Math.round(seconds);
	const h = Math.floor(total / 3600);
	const m = Math.floor((total % 3600) / 60);
	const s = total % 60;

	const pad = (n: number) => n.toString().padStart(2, '0');
	return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

/** Framerates arrive as exact rationals, so 29.97 must not render as "29.97002997". */
export function formatFps(fps: number): string {
	if (!Number.isFinite(fps) || fps <= 0) return '—';
	return Number.isInteger(fps) ? `${fps}` : fps.toFixed(2).replace(/0+$/, '').replace(/\.$/, '');
}

export function formatPercent(fraction: number): string {
	if (!Number.isFinite(fraction)) return '0%';
	return `${Math.round(Math.max(0, Math.min(1, fraction)) * 100)}%`;
}

/** "87% smaller" reads better than a ratio, and is the number people care about. */
export function formatReduction(from: number, to: number): string | null {
	if (!(from > 0) || !(to > 0) || to >= from) return null;
	return `${Math.round((1 - to / from) * 100)}% smaller`;
}
