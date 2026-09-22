#!/usr/bin/env bash
#
# Gets a fresh macOS or Linux machine ready to develop Media Compressor, then
# runs it. The Windows equivalent is scripts/setup.ps1.
#
#     ./scripts/setup.sh              install what is missing, then run the app
#     ./scripts/setup.sh --no-start   install only
#
# Installs only what is missing and leaves anything already present alone, so
# running it twice is harmless.

set -euo pipefail

# Node 20 is the floor Vite 6 and SvelteKit 2 support.
readonly MINIMUM_NODE_MAJOR=20

# pnpm-workspace.yaml here carries settings and no `packages` field, which older
# pnpm rejects outright as an invalid workspace. This is the version the file was
# authored against; anything below it fails before installing a single package.
readonly MINIMUM_PNPM_VERSION=10.33.0

no_start=0
for argument in "$@"; do
    case "$argument" in
        --no-start) no_start=1 ;;
        *) echo "Unknown option: $argument" >&2; exit 2 ;;
    esac
done

step() { printf '\n\033[36m==> %s\033[0m\n' "$1"; }
have() { printf '    \033[90m%s\033[0m\n' "$1"; }
note() { printf '    \033[33m%s\033[0m\n' "$1"; }
die()  { printf '\n\033[31m%s\033[0m\n' "$1" >&2; exit 1; }

exists() { command -v "$1" >/dev/null 2>&1; }

# True when $1 is at least $2, comparing the way version numbers actually order
# rather than the way strings do — 10.33.0 is above 9.1.1, not below it.
version_at_least() {
    [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" = "$2" ]
}

# Anything needing root gets it explicitly; running the whole script under sudo
# would leave every file it touches owned by root.
as_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    elif exists sudo; then
        sudo "$@"
    else
        die "This step needs root and sudo is not installed. Run it yourself: $*"
    fi
}

# ---------------------------------------------------------------------------

# Being in the wrong directory is the likeliest way to arrive here, and every
# tool downstream reports it as something else — pnpm blames a missing
# package.json, cargo blames a missing manifest. Say it once, plainly.
project_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

if [ ! -f "$project_root/package.json" ]; then
    die "This is not the Media Compressor checkout:

    $project_root

There is no package.json in it. From a folder you keep code in:

    git clone https://github.com/sak0x7d5/media-compressor.git
    cd media-compressor
    ./scripts/setup.sh"
fi

cd "$project_root"

case "$(uname -s)" in
    Darwin) platform=macos ;;
    Linux)  platform=linux ;;
    *)      die "$(uname -s) is not a platform this script knows. Install Node ${MINIMUM_NODE_MAJOR}+, pnpm ${MINIMUM_PNPM_VERSION}+ and rustup by hand, then run: pnpm install && pnpm tauri dev" ;;
esac

printf '\033[1mMedia Compressor — development setup\033[0m\n'
have "Project:  $project_root"
have "Platform: $platform"

# --- the C toolchain and the system libraries Tauri links against ------------

step 'Checking the system build dependencies'
if [ "$platform" = macos ]; then
    if xcode-select --print-path >/dev/null 2>&1; then
        have 'Xcode command line tools are present.'
    else
        note 'Installing the Xcode command line tools — accept the dialog that opens.'
        xcode-select --install || true
        note 'Re-run this script once that installer has finished.'
        exit 0
    fi
else
    # Tauri renders through the system webview on Linux, so its development
    # headers are a hard requirement rather than a nicety.
    if exists pkg-config && pkg-config --exists webkit2gtk-4.1 2>/dev/null; then
        have 'WebKitGTK development headers are present.'
    elif exists apt-get; then
        note 'Installing the WebKitGTK and build dependencies through apt.'
        as_root apt-get update
        as_root apt-get install -y \
            libwebkit2gtk-4.1-dev build-essential curl wget file \
            libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
    elif exists dnf; then
        note 'Installing the WebKitGTK and build dependencies through dnf.'
        as_root dnf install -y \
            webkit2gtk4.1-devel openssl-devel curl wget file \
            libappindicator-gtk3-devel librsvg2-devel
        as_root dnf group install -y "c-development"
    elif exists pacman; then
        note 'Installing the WebKitGTK and build dependencies through pacman.'
        as_root pacman -Syu --needed --noconfirm \
            webkit2gtk-4.1 base-devel curl wget file openssl \
            libappindicator-gtk3 librsvg
    else
        die "No apt, dnf or pacman here, so the system libraries need installing by hand.
Tauri needs the WebKitGTK 4.1 development headers, a C toolchain, and librsvg."
    fi
fi

# --- git --------------------------------------------------------------------

step 'Checking Git'
if exists git; then
    have "$(git --version) is already installed."
elif [ "$platform" = macos ]; then
    have 'Git ships with the Xcode command line tools.'
else
    note 'Installing Git.'
    if   exists apt-get; then as_root apt-get install -y git
    elif exists dnf;     then as_root dnf install -y git
    elif exists pacman;  then as_root pacman -S --needed --noconfirm git
    fi
fi

# --- node -------------------------------------------------------------------

node_major() {
    exists node || { echo 0; return; }
    node --version | sed 's/^v//' | cut -d. -f1
}

step 'Checking Node'
if [ "$(node_major)" -ge "$MINIMUM_NODE_MAJOR" ]; then
    have "Node $(node --version) is fine."
else
    [ "$(node_major)" -gt 0 ] && note "Node $(node --version) is older than the required v${MINIMUM_NODE_MAJOR}."

    # nvm rather than the distro package, which is frequently several major
    # versions behind, and rather than Homebrew, which is macOS only. It also
    # needs no root, so this cannot fight a system-managed Node.
    if [ ! -s "${NVM_DIR:-$HOME/.nvm}/nvm.sh" ]; then
        note 'Installing nvm to manage the Node version.'
        curl -fsSL https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash
    fi

    export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
    # shellcheck disable=SC1091
    . "$NVM_DIR/nvm.sh"
    nvm install --lts
    nvm use --lts

    [ "$(node_major)" -ge "$MINIMUM_NODE_MAJOR" ] ||
        die "Node is still $(node --version) after installing. Open a new terminal and run this script again."
fi

# --- pnpm -------------------------------------------------------------------

pnpm_version_of() {
    "$1" --version 2>/dev/null | tr -d '[:space:]'
}

# Everything downstream goes through this rather than through the bare name, so
# a stale pnpm winning PATH cannot decide what the rest of the script runs.
pnpm_command=pnpm

step 'Checking pnpm'
current_pnpm=''
exists pnpm && current_pnpm=$(pnpm_version_of pnpm)

if [ -n "$current_pnpm" ] && version_at_least "$current_pnpm" "$MINIMUM_PNPM_VERSION"; then
    have "pnpm $current_pnpm is fine."
else
    [ -n "$current_pnpm" ] &&
        note "pnpm $current_pnpm predates $MINIMUM_PNPM_VERSION and cannot read this project's settings; upgrading it."
    npm install --global pnpm@latest

    hash -r 2>/dev/null || true
    current_pnpm=''
    exists pnpm && current_pnpm=$(pnpm_version_of pnpm)

    # npm installed a current pnpm, but a standalone pnpm install, Volta, asdf or
    # Homebrew can sit earlier on PATH and keep answering to the name. Address
    # the new one directly rather than fighting over PATH, which is the user's
    # to reorder, not this script's.
    if [ -z "$current_pnpm" ] || ! version_at_least "$current_pnpm" "$MINIMUM_PNPM_VERSION"; then
        candidate="$(npm prefix --global 2>/dev/null)/bin/pnpm"
        candidate_version=''
        [ -x "$candidate" ] && candidate_version=$(pnpm_version_of "$candidate")

        if [ -n "$candidate_version" ] && version_at_least "$candidate_version" "$MINIMUM_PNPM_VERSION"; then
            note "The pnpm on PATH is still ${current_pnpm:-missing}, so this run uses the newer one directly:"
            note "  $candidate"
            note 'pnpm is installed in more than one place; PATH order decides which one runs:'
            for found in $(type -a -P pnpm 2>/dev/null | awk '!seen[$0]++'); do
                note "  $found  (v$(pnpm_version_of "$found"))"
            done
            note 'Until the older copy is removed, typing `pnpm` by hand keeps getting the old version.'
            pnpm_command="$candidate"
            current_pnpm="$candidate_version"
        else
            die "pnpm is still ${current_pnpm:-missing} after upgrading, and this project needs $MINIMUM_PNPM_VERSION or newer."
        fi
    fi
    have "Using pnpm $current_pnpm."
fi

# --- rust -------------------------------------------------------------------

step 'Checking Rust'
if exists cargo; then
    have "$(rustc --version) is already installed."
else
    note 'Installing Rust through rustup.'
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
    # rustup puts this on PATH for future shells only.
    export PATH="$HOME/.cargo/bin:$PATH"
    # shellcheck disable=SC1091
    [ -s "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
    exists cargo || die 'Rust did not install cleanly. Install it from https://rustup.rs and run this script again.'
fi

# --- the project itself -----------------------------------------------------

step 'Installing project dependencies'
"$pnpm_command" install

exists ffmpeg ||
    note 'No ffmpeg on PATH — the app downloads its own build (~80 MB) the first time it encodes something. Nothing to do.'

if [ "$no_start" -eq 1 ]; then
    step 'Ready'
    printf '    \033[32mRun `pnpm tauri dev` when you want the app.\033[0m\n'
    exit 0
fi

step 'Starting the app (pnpm tauri dev)'
have 'The first Rust build takes several minutes. Later ones are seconds.'
have 'Ctrl+C stops it. Edits to the UI reload on save.'
"$pnpm_command" tauri dev
