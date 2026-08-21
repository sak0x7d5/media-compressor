/** Job list state, and the translation from backend events into what a row shows. */

import type { EncodePlan, JobEvent, MediaInfo, MediaOutcome, QueuedFile, Stage } from './ipc';
import { formatBitrate, formatBytes, formatFps } from './format';

export type JobState = 'queued' | 'running' | 'done' | 'failed' | 'cancelled';

export interface Job {
	id: string;
	name: string;
	input: string;
	output: string;
	inputBytes: number;
	state: JobState;
	/** Short right-aligned status: "queued", "pass 1", "42%", "done". */
	status: string;
	/** Progress through the whole job, 0–1. */
	fraction: number;
	/** The monospace subtitle: what the encoder decided. */
	detail: string;
	outcome?: MediaOutcome;
	error?: string;
}

const CODEC_LABEL: Record<EncodePlan['codec'], string> = {
	h264: 'h264',
	hevc: 'h265',
	av1: 'av1'
};

/** "1080p60 → 854×480@30 · 2-pass · 732 kbps" — the reasoning, not just a number. */
export function describePlan(plan: EncodePlan, info?: MediaInfo): string {
	const parts: string[] = [];

	const target = `${plan.scale.width}×${plan.scale.height}@${formatFps(plan.scale.fps)}`;
	if (info && (info.width !== plan.scale.width || info.height !== plan.scale.height || info.fps > plan.scale.fps + 0.01)) {
		parts.push(`${info.width}×${info.height}@${formatFps(info.fps)} → ${target}`);
	} else {
		parts.push(target);
	}

	parts.push(CODEC_LABEL[plan.codec]);

	if (plan.rate_control.mode === 'two-pass') {
		parts.push('2-pass');
		parts.push(formatBitrate(plan.rate_control.video_bps));
	} else {
		parts.push(`crf ${plan.rate_control.crf}`);
	}

	if (plan.scale.below_floor) {
		parts.push('at quality floor');
	}

	return parts.join(' · ');
}

/** "1440×810 · webp · quality 74 · 6 encodes" — what the search settled on. */
export function describeImage(outcome: Extract<MediaOutcome, { kind: 'image' }>): string {
	const parts = [`${outcome.width}×${outcome.height}`, `quality ${outcome.quality}`];
	if (outcome.downscale_steps > 0) {
		parts.push(`downscaled ${outcome.downscale_steps}×`);
	}
	parts.push(`${outcome.encodes} encodes`);
	return parts.join(' · ');
}

function stageStatus(stage: Stage): { status: string; fraction?: number; detail?: string } {
	switch (stage.stage) {
		case 'probing':
			return { status: 'reading', fraction: 0 };
		case 'predicting':
			return { status: 'sampling', fraction: 0 };
		case 'planned':
			return { status: 'planned', detail: describePlan(stage.plan) };
		case 'encoding': {
			const label = stage.of_passes > 1 ? `pass ${stage.pass}/${stage.of_passes}` : 'encoding';
			return { status: label, fraction: stage.overall_fraction };
		}
		case 'correcting':
			return { status: `retry ${stage.attempt}`, detail: `overshot at ${formatBytes(stage.actual_bytes)}, re-encoding` };
		case 'searching':
			return {
				status: `try ${stage.encode}`,
				// Images search rather than predict, so the bar tracks probes.
				fraction: stage.encode / Math.max(stage.of, 1),
				detail: `quality ${stage.quality} → ${formatBytes(stage.bytes)}${stage.fits ? ' ✓' : ''}`
			};
	}
}

export class JobList {
	jobs = $state<Job[]>([]);

	get active(): Job | undefined {
		return this.jobs.find((job) => job.state === 'running');
	}

	get finished(): Job[] {
		return this.jobs.filter((job) => job.state === 'done');
	}

	get anyRunning(): boolean {
		return this.jobs.some((job) => job.state === 'running' || job.state === 'queued');
	}

	/** True when exactly one file is in play and it has finished — the result-card case. */
	get soleResult(): Job | undefined {
		if (this.jobs.length !== 1) return undefined;
		const only = this.jobs[0];
		return only.state === 'done' ? only : undefined;
	}

	add(files: QueuedFile[]) {
		for (const file of files) {
			if (this.jobs.some((job) => job.id === file.id)) continue;
			this.jobs.push({
				id: file.id,
				name: file.name,
				input: file.input,
				output: file.output,
				inputBytes: file.input_bytes,
				state: 'queued',
				status: 'queued',
				fraction: 0,
				detail: formatBytes(file.input_bytes)
			});
		}
	}

	remove(id: string) {
		this.jobs = this.jobs.filter((job) => job.id !== id);
	}

	clear() {
		this.jobs = this.jobs.filter((job) => job.state === 'running' || job.state === 'queued');
	}

	apply(event: JobEvent) {
		const job = this.jobs.find((candidate) => candidate.id === event.id);
		if (!job) return;

		switch (event.event) {
			case 'queued':
				break;

			case 'started':
				job.state = 'running';
				job.status = 'starting';
				job.fraction = 0;
				break;

			case 'progress': {
				job.state = 'running';
				const { status, fraction, detail } = stageStatus(event.stage);
				job.status = status;
				if (fraction !== undefined) job.fraction = fraction;
				if (detail !== undefined) job.detail = detail;
				break;
			}

			case 'finished': {
				job.state = 'done';
				job.status = 'done';
				job.fraction = 1;
				job.outcome = event.outcome;
				job.output = event.output;
				job.detail =
					event.outcome.kind === 'video'
						? describePlan(event.outcome.plan, event.outcome.info)
						: describeImage(event.outcome);
				break;
			}

			case 'failed':
				job.state = 'failed';
				job.status = 'failed';
				job.error = event.message;
				job.detail = event.message;
				break;

			case 'cancelled':
				job.state = 'cancelled';
				job.status = 'cancelled';
				job.detail = 'cancelled';
				break;
		}
	}
}
