# Changelog

This file is not just documentation — the app reads it. The newest entry is
compiled into the binary and shown as **What's new** the first time you run a
version you haven't seen, and the release workflow lifts the matching section
into the GitHub release and the in-app update prompt.

So write entries for the person about to click "Update", not for the commit
log. "Fixed a race in the queue worker" tells them nothing; "cancelling a job no
longer leaves the next one stuck at 0%" tells them whether they care.

Format is [Keep a Changelog](https://keepachangelog.com), versions are
[semantic](https://semver.org). Newest first — the parser trusts that order and
a test enforces it. Changes that have not shipped go under `[Unreleased]`; the
app ignores that entry until the heading becomes a version, which is what
`pnpm version:set` expects to find.

## [Unreleased]

### Added
- **Updates without reinstalling.** The app checks for a new release a few
  seconds after launch, shows what changed, and installs it in place when you
  say so. Downloads are signed with a key that never leaves the maintainer's
  machine, and an installer that fails the signature check is discarded rather
  than run.
- **A What's new panel** after an update lands, built from this file. It is
  compiled into the binary, so reading what changed needs no network access and
  works offline.
- **Settings → Updates**: turn the launch check off, check on demand, and read
  the full release history at any time. A check that finds nothing says so in
  plain words — "the server answered, but has no release to offer" is not the
  same as "couldn't reach the server", and the panel no longer guesses.
- **Stop keeps your queue.** Stop pauses instead of throwing everything away:
  the file being encoded goes back to the front of the line, and Resume picks
  up where you left off. Clear is what discards the queue.
- **Scrub the comparison.** The before/after view has a timeline; drag it to
  compare any moment in the clip, not just the middle.
- **Results can replace the originals.** Off by default. A replaced original
  goes to the recycle bin rather than being deleted, and a result that is not
  smaller than its source leaves the source alone.
- **A "Shrink" cascade in Explorer's right-click menu**, opening onto the
  preset sizes, so a file can be compressed to a given size in two clicks
  without opening the app first. The menu follows your presets, and
  uninstalling removes it.
- **Uses the FFmpeg you already have.** A build on PATH is adopted when it has
  every encoder the app needs, instead of downloading an 80 MB copy of what is
  already installed. Settings shows which is in use and lets you switch.
- `.tif` images are accepted alongside `.tiff`.

### Changed
- Faster corrective re-encodes: a second attempt reuses the analysis pass the
  first one already did instead of repeating it.
- The window appears with the interface already drawn, rather than as an empty
  frame that fills in.
- Settings asks FFmpeg for its version once, not every time the panel opens.
- The right-click menu no longer claims `.ts` files, which are TypeScript
  sources as often as they are transport streams. Transport streams can still
  be dragged in or opened from the browse dialog.

### Fixed
- Cancelling a job never leaves a half-written file behind.
- Two jobs from the same source no longer write over each other's results.
- Comparing before and after works when the encode came out shorter than its
  source, and says what went wrong when a frame cannot be read.
- Progress events that arrived before their row existed are no longer dropped.

### Note
- Updating *to* the first release that contains the updater is the last manual
  reinstall. Earlier builds have no updater in them, so they cannot pull a new
  version down by themselves.

## [0.1.0] — 2026-09-13

### Added
- Size-targeted video and image compression: name a size, get the best-looking
  file that fits under it, first try.
- Resolution and framerate fitted to the budget before encoding, so a 20 MB cap
  produces a clean 540p rather than a blocky 1080p.
- Two-pass rate control only when a sample encode says a plain CRF pass would
  bust the cap.
- Output measured after encoding, with up to two corrective re-encodes scaled by
  how far it missed.
- Binary-searched quality targeting for images, against the real encoder rather
  than an estimate.
- Upload limits as data — bundled, user-editable, and optionally refreshed from
  a URL you control.
- Drag and drop, an Explorer right-click entry, before/after frame comparison,
  and copy-to-clipboard for pasting straight into Discord.
- A choice of where results go: beside the original, in a folder of your own,
  or asked each time.
