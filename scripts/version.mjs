/**
 * Set the version everywhere at once.
 *
 *   pnpm version:set 0.3.0
 *
 * Four files carry the number — package.json, Cargo.toml, Cargo.lock and
 * tauri.conf.json — and the updater compares the version it downloads against
 * the one the running binary reports. Let those drift and the app either offers
 * an update it already has, forever, or refuses one it needs. Hence one command
 * rather than four edits.
 *
 * It refuses to run without a changelog entry for the new version, because the
 * release notes shown in the update prompt are lifted straight out of that
 * entry. A release with nothing to say about itself is a dialog that says
 * "Update available" and nothing else.
 */

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');

const version = process.argv[2];
if (!version) {
	fail('usage: pnpm version:set <version>   e.g. pnpm version:set 0.3.0');
}
if (!/^\d+\.\d+\.\d+$/.test(version)) {
	fail(`"${version}" is not a three-part version like 0.3.0`);
}

const changelog = read('CHANGELOG.md');
const heading = new RegExp(`^##\\s+\\[?${version.replace(/\./g, '\\.')}\\]?`, 'm');
if (!heading.test(changelog)) {
	fail(
		`CHANGELOG.md has no entry for ${version}.\n` +
			`Add one first — it becomes the release notes people read before updating:\n\n` +
			`  ## [${version}] — ${new Date().toISOString().slice(0, 10)}\n\n  ### Added\n  - …\n`
	);
}

/* Every file is edited in place rather than parsed and re-serialised. Round
   tripping tauri.conf.json through JSON.stringify reformats every array in it,
   which turns a version bump into a thirty-line diff nobody can review. */
const TOP_LEVEL_VERSION = /^(  "version": ")([^"]+)(")/m;

const edits = [
	replace('package.json', TOP_LEVEL_VERSION, 'the top-level version'),
	replace('src-tauri/tauri.conf.json', TOP_LEVEL_VERSION, 'the top-level version'),
	replace(
		'src-tauri/Cargo.toml',
		/(\[package\][\s\S]*?\nversion\s*=\s*")([^"]+)(")/,
		'the [package] version'
	),
	replace(
		'src-tauri/Cargo.lock',
		/(\[\[package\]\]\r?\nname = "media-compressor"\r?\nversion = ")([^"]+)(")/,
		'the locked package version'
	)
];

for (const edit of edits) edit();

console.log(`\nVersion set to ${version} in ${edits.length} files.\n`);
console.log('Next:');
console.log('  git commit -am "release: v' + version + '"');
console.log('  git tag v' + version + ' && git push --follow-tags');
console.log('\nThe tag is what triggers the build; nothing is published without one.');

function replace(relative, pattern, what) {
	return () => {
		const before = read(relative);
		if (!pattern.test(before)) {
			fail(`could not find ${what} in ${relative} — has its format changed?`);
		}
		let from = '';
		const after = before.replace(pattern, (_, head, current, tail) => {
			from = current;
			return `${head}${version}${tail}`;
		});
		write(relative, after);
		console.log(`  ${relative}: ${from} → ${version}`);
	};
}

function read(relative) {
	return readFileSync(join(root, relative), 'utf8');
}

function write(relative, contents) {
	writeFileSync(join(root, relative), contents);
}

function fail(message) {
	console.error(`\n${message}\n`);
	process.exit(1);
}
