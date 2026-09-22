/**
 * Turning release notes into something the UI can render.
 *
 * Two sources end up in the same shape. The What's new panel gets its entries
 * already structured from Rust, which parses the changelog compiled into the
 * binary. The update prompt gets a blob of text out of the update manifest,
 * which is why `parseNotes` exists — and why nothing here ever produces HTML.
 * Manifest notes arrive over the network; handing them to `{@html}` inside a
 * webview that can invoke commands would be a remote code path for the sake of
 * some bold text.
 */

import type { ChangeSection } from './ipc';

const DEFAULT_HEADING = 'Changes';

/**
 * Parse the notes attached to an update.
 *
 * Deliberately more forgiving than the Rust parser: this text can be edited by
 * hand on the release page, so a plain paragraph with no bullet in front of it
 * still has to show up rather than vanish.
 */
export function parseNotes(text: string): ChangeSection[] {
	const sections: ChangeSection[] = [];

	const current = (): ChangeSection => {
		if (sections.length === 0) sections.push({ heading: DEFAULT_HEADING, items: [] });
		return sections[sections.length - 1];
	};

	for (const line of text.split(/\r?\n/)) {
		const trimmed = line.trim();
		if (trimmed.length === 0) continue;

		const heading = /^#{1,6}\s+(.*)$/.exec(trimmed);
		if (heading) {
			sections.push({ heading: heading[1].trim(), items: [] });
			continue;
		}

		const bullet = /^[-*]\s+(.*)$/.exec(trimmed);
		if (bullet) {
			current().items.push(bullet[1].trim());
			continue;
		}

		// Indentation continues the item above — the changelog is hard wrapped,
		// so half of every long entry lives on a second line.
		const section = current();
		const last = section.items.length - 1;
		if (/^\s/.test(line) && last >= 0) {
			section.items[last] = `${section.items[last]} ${trimmed}`;
		} else {
			section.items.push(trimmed);
		}
	}

	return sections.filter((section) => section.items.length > 0);
}

export type Span = { kind: 'text' | 'strong' | 'code'; text: string };

/**
 * Split `**bold**` and `` `code` `` out of an item so the markup can be built
 * from real elements instead of injected as a string.
 */
export function spans(text: string): Span[] {
	const out: Span[] = [];

	for (const piece of text.split(/(\*\*[^*]+\*\*|`[^`]+`)/g)) {
		if (piece.length === 0) continue;

		if (piece.startsWith('**') && piece.endsWith('**') && piece.length > 4) {
			out.push({ kind: 'strong', text: piece.slice(2, -2) });
		} else if (piece.startsWith('`') && piece.endsWith('`') && piece.length > 2) {
			out.push({ kind: 'code', text: piece.slice(1, -1) });
		} else {
			out.push({ kind: 'text', text: piece });
		}
	}

	return out;
}
