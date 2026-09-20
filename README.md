# Too ~~broke~~... I mean, too smart not to buy Nitro? I GOTCHA!

Compress. Upload. Fail. Lower the bitrate. Repeat.

**Or — drop it here once.** Media Compressor takes a video or an image and a
target size, and hands back the best-looking file that fits under it. First try.

Built because Discord caps attachments and keeps moving the cap: 25 MB → 8 → 25
→ 10 → **20 MB (13 August 2026)**. Nothing about this app is Discord-specific
though; Discord is just the loudest preset.

---

## "Why not just use one of those websites?"

Because to compress a 400 MB clip on a website, you have to upload 400 MB first.

That restates the problem rather than solving it. You have a file that is too big
to send, and the proposed fix is to send it somewhere else — over the slowest
link you own. Home connections upload several times slower than they download,
so a few hundred megabytes is minutes of progress bar *before any compression
has started*. Then a queue. Then a download.

This encodes on the machine the file is already on. Nothing is uploaded.

### The caveats stack up from there

| | Browser-based compressor | This |
|---|---|---|
| Before it starts | Upload the entire file | Nothing — the file is already here |
| Input size limit | Usually capped, often below the sizes that need compressing | Whatever your disk holds |
| What you ask for | A quality preset, or a percentage | The exact number of bytes |
| If it misses the cap | Lower a setting, re-upload, wait again | Measured and corrected automatically |
| Resolution | Left alone, bitrate starved until it blocks | Fitted to the budget before encoding starts |
| Several files | One at a time | A queue |
| Privacy | Your footage sits on a stranger's server | Never leaves the machine |
| Cost | Free tier, a watermark, or a subscription | Free |

The input cap is the one worth dwelling on: **a compressor that rejects files
over 100 MB only works on files you did not need to compress.** It fails at
precisely the moment it becomes useful.

In fairness, a few sites do let you type a target size. Almost none of them then
*change the picture to fit it* — they hold your resolution and starve the
bitrate, which is how you get 1080p that looks like wet paper. That decision is
the entire subject of the next section.

### Where a website stops, and this doesn't

A website hands you a file in your Downloads folder. You still have to find it,
drag it into Discord, and hope you grabbed the right one of the four you made.

- **Copy** puts the result on the clipboard *as a file*. Ctrl+V in the Discord
  message box attaches it — no folder, no dragging.
- **Right-click → Compress for Discord** in Explorer, and it never has to be
  opened in anything at all.
- **Drop in twelve clips and walk away.** They encode one after another, because
  two at once finish no sooner — they only make both progress bars lie.

### Use something else if

- **You are on a phone, a Chromebook, or a locked-down work machine.** A website
  runs anywhere. This needs Windows and an install, and that is a real advantage
  for the website, not a small one.
- **You compress one file a year.** An install — plus an 80 MB FFmpeg download,
  if you don't already have one — to save a single upload is a bad trade.
- **You want the controls, not the answer.** Handbrake exposes every knob this
  decides for you. If you already know which ones you want, use it.

### And Nitro?

Nitro raises the ceiling. It does not make anything smaller. It is a monthly fee,
it has a limit of its own that you will eventually hit, and it does nothing for
every other place you have to fit a file under a number. If you already pay for
it, enjoy — you still cannot send a 400 MB clip.

---

## What makes it different

Most size-targeting tools do one of two things badly. They guess a bitrate and
overshoot, so you re-encode three times. Or they aim so far under the cap that
you send a mushy file when 19.9 MB was available.

This one treats the cap as a budget to *spend*, not a wall to hide from.

**1. It fits the picture to the budget before it encodes anything.**
This is the biggest quality lever and the step most tools skip. 1080p at 900 kbps
isn't soft 1080p — it's blocking and smear. The same 900 kbps at 540p is a clean
540p. The app computes bits per pixel (`bitrate / (width × height × fps)`), walks
a resolution ladder, and picks the largest picture that still clears the codec's
quality floor. Resolution is spent down to 480p before framerate is touched at
all, because below that, lost detail hurts more than judder does.

**2. It only uses two-pass when it has to.**
Two-pass is *rate control*, not a quality mode. If a short sample encode predicts
the file already fits at a quality target, it ships a plain CRF encode — which
spends *fewer* bits than the budget and looks better per byte than any VBR pass.
Two-pass only comes out when the file would otherwise bust the cap.

**3. It checks its work.**
The output is measured. Over the limit → one corrective re-encode scaled by how
far it missed, with audio excluded from the ratio because audio is a fixed cost.
Capped at two corrections; an unbounded bisection converges beautifully and turns
a 40-second job into four minutes.

**4. Images get a real search, not an estimate.**
An image encode takes milliseconds, so there's nothing to predict — it binary
searches the quality parameter against the actual encoder until the file lands
just under target, then downscales only if no quality setting fits.

---

## Codec defaults, and why

| Target | Default | Reason |
|---|---|---|
| Discord video | **H.264** | Discord disabled AV1 video embeds — their servers can't generate a thumbnail for it, so an AV1 file posts as a bare download link with no inline player. |
| Other video | AV1 (opt-in) | ~25–30% fewer bits than H.265 at equal quality. Much slower. |
| Images | **WebP** | Discord added native WebP and AVIF support. WebP encodes in milliseconds; AVIF is smaller but seconds per image, and the search does that seven times. |

Audio matters more than it looks: at a 20 MB cap on a three-minute clip, 128 kbps
stereo is 15% of the whole file. Default is AAC 96k, dropping down a ladder
before video is allowed to fall below its quality floor.

Hardware encoders (NVENC/QSV/AMF) are deliberately **not** the default. They're
faster and meaningfully worse per bit, which is the wrong trade when the entire
job is fitting under a cap.

---

## Upload limits

Limits live in data, not code, in three layers:

1. a built-in list compiled into the binary, which can never fail to load;
2. a user copy in the config directory, which wins when present;
3. optionally, a URL you control, re-checked once a day.

**With no URL configured — the default — the app makes no network requests for
presets at all.** Set one in Settings and a platform changing its cap stops
needing a new release. Fetched lists are validated, not trusted: https only,
size-capped, and every entry checked for sane sizes and unique ids before it is
allowed to replace anything.

Every target aims at 95% of the stated limit by default. "20 MB" is ambiguous
between 20,000,000 and 20,971,520 bytes and platforms disagree about which they
mean; nobody has ever been annoyed that a file came out at 19.2 MB.

---

## Where results go

Four choices, in Settings:

| Mode | Result | Original |
|---|---|---|
| Next to the original *(default)* | `clip (compressed).mp4` | untouched |
| In a folder I choose | `clip.mp4`, in that folder | untouched |
| Ask me each time | `clip.mp4`, in the folder picked per batch | untouched |
| Replace the original | `clip.mp4`, where `clip.mp4` was | **recycle bin** |

A result landing in a folder of its own keeps the source's exact name; sharing a
folder with the source it has to be distinguished, hence the suffix. Names
matter here because Discord shows the filename to everyone in the channel.

**Replacing gives up files you already had, and lossy compression is not
reversible.** It is off by default, asks for confirmation the first time it is
switched on, and the footer says `replacing originals` for as long as it stays
on. What it does not do:

- It never encodes over the file it is reading. The encode goes to a scratch
  file beside the original, and only a *finished* encode is moved into place —
  a crash, a cancel, or a failed encode leaves the original as it was.
- It never touches a file that isn't the one being compressed. Compressing
  `clip.mov` next to an unrelated `clip.mp4` steps around the `clip.mp4` rather
  than consuming it.
- It never trades a file for a worse one. A result that comes out no smaller
  than its source is discarded and the original left alone — that outcome would
  be larger *and* re-encoded.
- It never deletes outright. Replaced originals go to the recycle bin, because
  the file standing in for them is lossy and may be the only copy there was. A
  bin that refuses the file stops the swap rather than falling back to erasing
  it. Emptying the bin is still final, so keep anything you cannot re-download
  backed up elsewhere.

A format change replaces across the extension: compressing `clip.mov` leaves
`clip.mp4` and no `clip.mov`. Comparing before and after is unavailable for a
replaced file, because there is no longer a "before" to read.

---

## FFmpeg

FFmpeg does all the actual encoding, and it isn't bundled. Three places are
searched, in this order:

1. **The app's own copy**, in its app-data folder. Whatever an existing install
   is already using stays in use — an ordinary launch never switches encoders
   behind your back.
2. **`MEDIA_COMPRESSOR_FFMPEG`**, a directory containing `ffmpeg` and `ffprobe`.
   Exempt from the check below: its whole purpose is pointing the app at an
   unusual build deliberately.
3. **Your PATH** — an FFmpeg you already have.

Only if none of those turn anything up does it download one (~80 MB), into its
own folder, verifying the binaries execute before accepting the install.

### On trusting the FFmpeg you already have

Builds differ in which encoders were compiled in, and one missing encoder
otherwise surfaces minutes later as a dead job on a file you already dropped. So
a build on PATH is adopted only after being asked, out loud, whether it has
every encoder this app can ask for — `libx264`, `libx265` and `libsvtav1`, a
list derived from `VideoCodec` so it cannot drift.

That costs one `ffmpeg -encoders` the first time and nothing afterwards: the
answer is cached in `system.json` next to the binary's size and timestamp, and
a build that changes underneath us fails that match and gets asked again. If
your FFmpeg doesn't make the bar, the first-run screen says which encoder it
lacks rather than downloading in silence.

Settings shows which of the three is in use and lets you move between the two
that are yours to choose: download a private copy while using your own, or
switch to your own and delete the private copy.

### On integrity

The upstream download URL is a rolling "latest" build with no published
per-build hash, so a pinned checksum isn't achievable against it. What *is*
enforced is HTTPS, and that the extracted binaries actually run — which is what
catches a truncated or corrupted download. The archive hash and resolved version
are recorded in `install.json` next to the binaries so a later launch can detect
the install changing underneath it.

The archive also carries `ffplay`, which this app never launches; it is deleted
after unpacking rather than left as another 90 MB of someone's disk.

---

## Releases and updating

The app can update itself: it asks the release host what the newest version is
and installs it, so nobody has to go and fetch an installer again. Every update
must carry a signature made with this project's private key, and the matching
public key is compiled into the binary — a compromised download host cannot push
anything the app will accept.

Publishing a release is a version bump and a tag:

```bash
# The version in these three files is what the release actually contains.
#   src-tauri/tauri.conf.json   ← the one that reaches latest.json
#   src-tauri/Cargo.toml
#   package.json
git commit -am "release v0.2.0"
git tag v0.2.0 && git push origin main v0.2.0
```

That triggers a workflow which builds the installer, signs it, and publishes a
draft release including a `latest.json` describing the new version. Installed
copies check that file and offer the update.

The tag has to match the version in `tauri.conf.json`, and the workflow stops if
it doesn't. That version — not the tag — is what `latest.json` advertises, so
tagging `v0.2.0` on a tree still saying `0.1.0` would publish a release every
installed copy reads as "nothing newer here". It is the one mistake this process
can make silently, which is why it is checked rather than documented and hoped
for.

Running the workflow by hand from the Actions tab builds the installer without
publishing anything, which is the way to check it works before committing to a
tag.

**One thing must be true before the first release:**

The **`TAURI_SIGNING_PRIVATE_KEY` secret must be set** to the contents of the
private key file, under Settings → Secrets and variables → Actions. The key
lives outside this repository by design and must never be committed; losing it
means existing installs can never be updated again, because they will reject
anything signed with a different key.

The repository also has to be public, so an installed copy can fetch
`latest.json` without a login — that part is already done.

Until the secret is set, the in-app check simply reports that it could not reach
the update server, which is accurate and harmless.

**Update support only works forward.** A copy of the app can only update itself
if the build the user installed already contained the updater. Anyone running a
build from before this was added has to install once manually.

---

## Development

Requires Rust (stable), Node 20+, pnpm, and the MSVC C++ build tools on Windows.

```bash
pnpm install
pnpm tauri dev
```

Tests:

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

The unit tests are pure — no FFmpeg, no I/O, no async — because that is where
the quality decisions live and they have to be checkable without spawning a
process. The integration tests in `src-tauri/tests/end_to_end.rs` run real
encodes; they generate their own source clips rather than checking a binary into
the repo, and skip with a printed note when FFmpeg can't be found.

```
src-tauri/src/
├── strategy/       budget, resolution ladder, CRF-vs-two-pass decision (pure)
├── ffmpeg/         locating, acquiring, probing, encoding, sampling
├── images.rs       binary-search quality targeting
├── pipeline.rs     probe → plan → encode → verify → correct
├── queue.rs        serial video, per-job cancellation
└── commands.rs     the surface the UI calls
```

---

## Licensing note

FFmpeg builds that include x264 and x265 are GPL. This app calls a separate
`ffmpeg` executable rather than linking the libraries, which keeps the two at
arm's length. Statically linking libav into this binary would put the whole app
under the GPL — worth knowing before changing how FFmpeg is invoked.
