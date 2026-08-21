# Media Compressor

Drop a video or image in, name a size, get the best-looking file that fits under
it — first try.

Built because Discord caps attachments and keeps moving the cap: 25 MB → 8 → 25
→ 10 → **20 MB (13 August 2026)**. Nothing about this app is Discord-specific
though; Discord is just the loudest preset.

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

## FFmpeg

FFmpeg does all the actual encoding. It isn't bundled — on first run the app
downloads a build (~80 MB) into its own app-data folder and verifies the
binaries execute before accepting the install.

On integrity: the upstream download URL is a rolling "latest" build with no
published per-build hash, so a pinned checksum isn't achievable against it. What
*is* enforced is HTTPS, and that the extracted binaries actually run — which is
what catches a truncated or corrupted download. The archive hash and resolved
version are recorded in `install.json` next to the binaries so a later launch can
detect the install changing underneath it.

Set `MEDIA_COMPRESSOR_FFMPEG` to a directory containing `ffmpeg` and `ffprobe` to
use a specific build instead. Debug builds additionally fall back to PATH, so
`pnpm tauri dev` works without waiting for a download.

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
