# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## The app in one paragraph

A Tauri 2 desktop app (Windows-first): SvelteKit 5 static SPA in `src/`, Rust
backend in `src-tauri/` that drives FFmpeg as a **child process**. Give it a
file and a size cap; it produces the best-looking file under that cap on the
first try. Everything interesting is the decision-making, not the encoding.
Read the README first — it is the design rationale, not marketing.

## Commands

`cargo`/`rustc` are **not on PATH** in the agent shell. Prefix once per shell:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

`node`, `pnpm` and a system `ffmpeg` are already on PATH.

| Task | Command |
|---|---|
| Install deps | `pnpm install` |
| Run the app | `pnpm tauri dev` |
| Build the installer (NSIS) | `pnpm tauri build` |
| Frontend alone (no Tauri APIs) | `pnpm dev` — fixed port 1420, `strictPort` |
| Typecheck Svelte + TS | `pnpm check` |
| All Rust tests | `cargo test --manifest-path src-tauri/Cargo.toml` |
| Unit tests only (pure, fast) | `cargo test --manifest-path src-tauri/Cargo.toml --lib` |
| Real-encode tests only | `cargo test --manifest-path src-tauri/Cargo.toml --test end_to_end` |
| One test | `cargo test --manifest-path src-tauri/Cargo.toml <name-substring>` |

The integration tests generate their own source clips and **skip silently**
when FFmpeg is missing — add `-- --nocapture` to see whether they actually ran
rather than assuming green means covered.

`MEDIA_COMPRESSOR_FFMPEG=<dir containing ffmpeg+ffprobe>` points any build at a
specific FFmpeg. Every build also looks on PATH and adopts what it finds if it
has every encoder the app can ask for (`ffmpeg/system.rs`), which is why
`pnpm tauri dev` works before the first-run download.

**Do not run `cargo fmt`.** There is no `rustfmt.toml` and the code is written
wider than rustfmt's defaults, so a format pass rewrites nearly every file.
Match the surrounding style by hand.

## Architecture

One file's journey, and where each step lives:

```
probe → plan → (sample → predict) → encode → measure → correct
ffmpeg/probe  strategy/   ffmpeg/sample   ffmpeg/encode   strategy/plan
                                    all sequenced by pipeline.rs
```

- **`src-tauri/src/strategy/`** — the pure core. `budget.rs` splits the byte
  cap into video/audio bitrate, `ladder.rs` picks the resolution and framerate
  that clear the codec's bits-per-pixel floor, `plan.rs` chooses CRF vs
  two-pass and computes corrections. No I/O, no FFmpeg, no async, by design:
  this is where quality is decided, so it must be testable without spawning a
  process. **Its tests are the specification** — change behaviour here and the
  test that fails is telling you which trade-off you just reversed.
- **`src-tauri/src/ffmpeg/`** — everything impure. `tools.rs` locates the
  binaries (app download → env override → PATH) and verifies them by
  *executing* them; `system.rs` decides whether a PATH build carries every
  encoder we need and caches the answer; `acquire.rs` downloads and installs; `probe.rs` reads
  source metadata; `sample.rs` encodes three short slices to predict a CRF
  encode's full size; `encode.rs` builds argument vectors as a pure function
  (so two-pass logs and scale filters are assertable) and runs them.
- **`pipeline.rs`** — sequences the above for one file and reports every
  decision through an `on_stage` callback so the UI can show reasoning.
- **`queue.rs`** — one OS worker thread; videos run strictly serially because
  FFmpeg already saturates every core. All scheduling state — the waiting
  list, the running job and its cancel token, the paused flag — sits under one
  lock, so "is this id waiting or already running?" is a read, not a race.
  Stop *pauses*: the running encode is aborted and re-queued at the head;
  Cancel discards. Either way every job ends in a reported terminal state.
- **`changelog.rs` / `updates.rs`** — `CHANGELOG.md` is `include_str!`'d and
  parsed (a test pins its newest entry to the build version, so unshipped
  work goes under `[Unreleased]`); the updater drives `tauri-plugin-updater`
  and decides which entries this profile has not yet been shown.
- **`commands.rs`** — the whole `#[tauri::command]` surface plus `AppState`
  (tools, queue, cache/config/work dirs). Registered in `lib.rs`.
- **`presets.rs`** — upload limits as layered data: compiled-in list →
  user file in the config dir → optional refresh from a user-set https URL.
  With no URL configured (the default) the app makes **no** network requests.

Frontend: `src/lib/ipc.ts` is the only file that touches `invoke`/`listen`;
`src/lib/jobs.svelte.ts` turns backend events into what a row displays (Svelte
5 runes, `$state`); `src/routes/+page.svelte` is the whole screen. SSR is off
(`+layout.ts`), adapter-static with an `index.html` fallback.

### The IPC contract is hand-mirrored

`src/lib/ipc.ts` restates the Rust types in TypeScript and **nothing checks
that they agree**. When you change a serialized Rust type, update `ipc.ts` in
the same commit. Watch the serde attributes: `Stage` is tagged on `stage`,
`JobEvent` on `event`, `MediaOutcome` on `kind`, all `rename_all =
"kebab-case"`. Event channel names are constants on both sides (`EVENT_JOB`,
`EVENT_INSTALL` in `commands.rs`; `EVENT_OPEN_FILES` in `lib.rs`;
`EVENT_UPDATE` in `updates.rs`).

### Windows specifics

`tauri_plugin_single_instance` must stay registered **first** in `lib.rs` — a
second launch from the Explorer context menu has to be intercepted before it
builds a window. `ffmpeg::hide_console` sets `CREATE_NO_WINDOW` on every child
so a queue of files doesn't strobe. `shell_integration.rs` writes the
right-click verb under `HKCU\Software\Classes` per extension (never `*`);
`register`/`unregister` are exact inverses.

## Constraints that are decisions, not accidents

Before "simplifying" any of these, read the comment above it:

- `strategy/` stays free of I/O, FFmpeg and async.
- FFmpeg is invoked as a separate executable. Linking libav would put the whole
  app under the GPL.
- A PATH FFmpeg is adopted only after it proves it has every encoder we use —
  system builds differ in which encoders were compiled in, and trusting one
  blindly turns a missing encoder into an unreproducible bug report. The env
  override is deliberately exempt from that gate.
- Attempt caps exist for wall-clock reasons: `MAX_ATTEMPTS = 3` (an unbounded
  bisection turns a 40-second job into four minutes), `MAX_SEARCH_STEPS = 7`
  for images.
- Every target aims at 95% of the stated limit; "20 MB" is ambiguous between
  10^6 and 2^20 units and platforms disagree.
- H.264 and WebP are the defaults for compatibility reasons documented in the
  README, not because they compress best.

## Comment style

Comments here explain *why*, and constants carry the measurement that justifies
them (`0.055` bpp is annotated with the 1080p30 encode it was derived from).
Match that. A comment restating the code is worse than none; a magic number
without its rationale will be "cleaned up" by the next reader.

## Git workflow

This is open source, so the history is part of the product. Commits are the
public record of *why* something is the way it is.

**Branch per feature.** Never commit directly to `main`. Branch first:
`feat/resolution-ladder`, `fix/two-pass-log-collision`,
`refactor/queue-cancellation`.

**Commit as you go, not at the end.** Each meaningful working change gets its
own commit: a commit should build, pass `cargo test --lib`, and be revertable
on its own. Do not accumulate a feature's worth of work and dump it in one
commit; do not commit a broken intermediate state either. One logical change
per commit — never mix a refactor with a behaviour change, because then neither
can be reviewed nor reverted.

Committing on a feature branch is expected and does not need to be asked about.
**Pushing does** — ask before any `git push`; it is not on the allow-list in
`.claude/settings.json`, so it prompts rather than runs.

**Conventional Commits**, matching what is already in the log:

```
<type>(<optional scope>): <imperative subject, lowercase, no period, ≤72 chars>

Body wrapped at 72–76 columns. Say why this change exists and what it
rejected, not what the diff already shows. Bullet lists are fine for
multi-part changes.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
```

Types: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `build`, `chore`.
Scopes track the module: `strategy`, `ffmpeg`, `queue`, `presets`, `images`,
`ui`, `shell`. A body is optional for a one-line `chore`; it is not optional
for anything that changes a quality trade-off — that reasoning is the only
place it gets written down.

Never commit: `.claude/settings.local.json` (machine-specific paths,
gitignored), generated test media under `tests/assets/`, or scratch encodes.
Lock files are marked `-diff` in `.gitattributes` — regenerate them, don't
hand-edit.
