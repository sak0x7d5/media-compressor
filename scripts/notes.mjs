/**
 * Print one version's changelog entry.
 *
 *   pnpm notes            # the version in package.json
 *   pnpm notes 0.3.0
 *
 * The release workflow pipes this into the GitHub release body, and Tauri
 * copies that into the update manifest — so this output is what a user reads in
 * the "What's changed" panel before deciding to install. Same words as the
 * changelog, by construction, rather than by someone remembering to paste.
 */

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');

const version =
	process.argv[2]?.replace(/^v/, '') ??
	JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).version;

const changelog = readFileSync(join(root, 'CHANGELOG.md'), 'utf8');
const lines = changelog.split(/\r?\n/);

const escaped = version.replace(/\./g, '\\.');
const start = lines.findIndex((line) => new RegExp(`^##\\s+\\[?${escaped}\\]?`).test(line));

if (start === -1) {
	console.error(`CHANGELOG.md has no entry for ${version}.`);
	process.exit(1);
}

const rest = lines.slice(start + 1);
const end = rest.findIndex((line) => /^##\s/.test(line));
const body = (end === -1 ? rest : rest.slice(0, end)).join('\n').trim();

if (body.length === 0) {
	console.error(`The ${version} entry in CHANGELOG.md is empty.`);
	process.exit(1);
}

console.log(body);
