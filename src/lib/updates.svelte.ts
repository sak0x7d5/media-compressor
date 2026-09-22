/**
 * The state machine behind the update bar.
 *
 * The backend does the work — fetch the manifest, verify the installer's
 * signature against the public key compiled into the app, run it, restart —
 * and explains a check that came back empty in plain words. What this adds is
 * manners: a failed background check is silent, an offered update can be waved
 * away for the session, and nothing installs itself out from under a running
 * encode.
 */

import { checkForUpdate, installUpdate, onUpdateProgress } from './ipc';
import { parseNotes } from './changelog';
import type { ChangeSection } from './ipc';

export type UpdatePhase =
	/** Nothing has been checked, or the check found nothing worth saying. */
	| 'idle'
	| 'checking'
	/** Checked on request, already current. Worth showing; not worth a banner. */
	| 'current'
	| 'available'
	| 'downloading'
	/** Downloaded and verified; the installer is running. On Windows it closes
	    the app to replace it, so this is usually the last phase anyone sees. */
	| 'installing'
	| 'error';

export class Updater {
	phase = $state<UpdatePhase>('idle');
	version = $state<string | null>(null);
	notes = $state<ChangeSection[]>([]);
	receivedBytes = $state(0);
	totalBytes = $state<number | null>(null);
	message = $state<string | null>(null);
	/** Waved away for this session. Asking again next launch is enough. */
	dismissed = $state(false);

	/**
	 * Should the bar be on screen?
	 *
	 * A failed *check* is Settings' problem — the user is looking at Settings,
	 * that is where they asked. A failure with an offer already in hand happened
	 * during an install the user watched start, and belongs where they were
	 * watching. `version` is set only once something has been offered, which is
	 * exactly that distinction.
	 */
	get offering(): boolean {
		if (this.dismissed) return false;
		if (this.phase === 'error') return this.version !== null;
		return this.phase === 'available' || this.busy;
	}

	get busy(): boolean {
		return this.phase === 'downloading' || this.phase === 'installing';
	}

	get fraction(): number | null {
		if (!this.totalBytes) return null;
		return Math.min(1, this.receivedBytes / this.totalBytes);
	}

	/**
	 * Ask whether a newer release exists.
	 *
	 * A background check that fails — no network, GitHub having a bad day, no
	 * release published yet — leaves no trace. There is nothing the user can do
	 * about it and nothing they lose by not knowing. A check they asked for
	 * reports honestly, in the backend's words: "the server answered, but has
	 * no release to offer" is not the same as "couldn't reach the server".
	 */
	async check(manual = false) {
		if (this.busy || this.phase === 'checking') return;

		this.phase = 'checking';
		this.message = null;

		try {
			const update = await checkForUpdate();

			if (!update) {
				this.version = null;
				this.phase = manual ? 'current' : 'idle';
				return;
			}

			this.version = update.version;
			this.notes = parseNotes(update.notes ?? '');
			this.dismissed = false;
			this.phase = 'available';
		} catch (error) {
			this.message = String(error);
			this.phase = manual ? 'error' : 'idle';
		}
	}

	/**
	 * Download the offered release and hand it to the installer.
	 *
	 * On success the backend restarts the app into the new version, so the
	 * `await` never returns; it comes back only to report a failure.
	 */
	async install() {
		if (this.version === null || this.busy) return;

		this.phase = 'downloading';
		this.receivedBytes = 0;
		this.totalBytes = null;
		this.message = null;

		const stop = await onUpdateProgress((event) => {
			switch (event.step) {
				case 'downloading':
					this.receivedBytes = event.received_bytes;
					this.totalBytes = event.total_bytes;
					break;
				case 'installing':
					this.phase = 'installing';
					break;
			}
		});

		try {
			await installUpdate();
		} catch (error) {
			this.phase = 'error';
			this.message = String(error);
		} finally {
			stop();
		}
	}

	/** Whatever failed, try that again rather than starting over. */
	async retry() {
		if (this.version !== null) await this.install();
		else await this.check(true);
	}

	dismiss() {
		this.dismissed = true;
		if (this.phase === 'current' || this.phase === 'error') this.phase = 'idle';
	}
}
