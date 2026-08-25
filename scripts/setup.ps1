#Requires -Version 5.1
<#
.SYNOPSIS
    Gets a fresh Windows machine ready to develop Media Compressor, then runs it.

.DESCRIPTION
    Installs only what is missing — Node, pnpm, Rust, the MSVC C++ build tools
    and the WebView2 runtime — using winget, then fetches the project's
    dependencies and starts the app. Anything already present is left alone, so
    running this twice is harmless.

.EXAMPLE
    ./scripts/setup.ps1

.EXAMPLE
    ./scripts/setup.ps1 -NoStart
    Sets everything up but does not launch the app.
#>
[CmdletBinding()]
param(
    # Set up the machine but stop short of running `pnpm tauri dev`.
    [switch]$NoStart
)

$ErrorActionPreference = 'Stop'

# Node 20 is the floor Vite 6 and SvelteKit 2 support.
$MinimumNodeMajor = 20

function Write-Step  { param([string]$Message) Write-Host "`n==> $Message" -ForegroundColor Cyan }
function Write-Have  { param([string]$Message) Write-Host "    $Message" -ForegroundColor DarkGray }
function Write-Note  { param([string]$Message) Write-Host "    $Message" -ForegroundColor Yellow }

function Test-Have {
    param([string]$Name)
    [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

# An installer writes the new directory to the persisted PATH, which this
# already-running shell never re-reads. Pulling it back in is what lets the
# script use a tool it installed a moment ago without asking for a new window.
function Update-SessionPath {
    $parts = @(
        [Environment]::GetEnvironmentVariable('Path', 'Machine')
        [Environment]::GetEnvironmentVariable('Path', 'User')
    ) | Where-Object { $_ }
    $env:Path = $parts -join ';'
}

function Install-WithWinget {
    param(
        [string]  $Id,
        [string]  $Label,
        [string[]]$ExtraArguments = @(),
        [string]  $Verify
    )

    Write-Step "Installing $Label"

    $wingetArguments = @(
        'install', '--id', $Id, '--exact', '--source', 'winget',
        '--accept-package-agreements', '--accept-source-agreements'
    ) + $ExtraArguments

    & winget @wingetArguments
    $code = $LASTEXITCODE

    # 3010 is "installed, wants a reboot" — true of the C++ build tools often
    # enough that treating it as failure would strand people who are fine.
    if ($code -eq 3010) {
        Write-Note "$Label installed but Windows wants a restart before it is fully registered."
    }

    Update-SessionPath

    if ($Verify -and -not (Test-Have $Verify)) {
        throw "$Label did not install cleanly (winget exit code $code). Install it by hand and run this script again."
    }
}

function Get-NodeMajorVersion {
    if (-not (Test-Have 'node')) { return 0 }
    $version = (& node --version) -replace '^v', ''
    return [int]($version.Split('.')[0])
}

# The C++ toolchain has no command on PATH to probe, so ask the Visual Studio
# installer's own locator whether any install carries the x64 tools.
function Test-MsvcBuildTools {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $false }

    $found = & $vswhere -products '*' -latest -prerelease `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
        -property installationPath
    return [bool]$found
}

function Test-WebView2 {
    # Present on every Windows 11 and on most Windows 10 machines; the key is
    # written under WOW6432Node on 64-bit Windows.
    $keys = @(
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
        'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
        'HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    )
    foreach ($key in $keys) {
        if (Test-Path $key) { return $true }
    }
    return $false
}

# ---------------------------------------------------------------------------

$projectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $projectRoot

Write-Host 'Media Compressor — development setup' -ForegroundColor White
Write-Have "Project: $projectRoot"

if (-not (Test-Have 'winget')) {
    throw @'
winget is not available, so this script cannot install anything.

Install "App Installer" from the Microsoft Store (or update Windows), reopen
PowerShell, and run this script again.
'@
}

Write-Step 'Checking Node'
$nodeMajor = Get-NodeMajorVersion
if ($nodeMajor -ge $MinimumNodeMajor) {
    Write-Have "Node $(& node --version) is fine."
} else {
    if ($nodeMajor -gt 0) {
        Write-Note "Node $(& node --version) is older than the required v$MinimumNodeMajor."
    }
    Install-WithWinget -Id 'OpenJS.NodeJS.LTS' -Label 'Node.js LTS' -Verify 'node'
}

Write-Step 'Checking pnpm'
if (Test-Have 'pnpm') {
    Write-Have "pnpm $(& pnpm --version) is already installed."
} else {
    & npm install --global pnpm
    if ($LASTEXITCODE -ne 0) { throw 'Could not install pnpm through npm.' }
    Update-SessionPath
}

Write-Step 'Checking Rust'
if (Test-Have 'cargo') {
    Write-Have "$(& rustc --version) is already installed."
} else {
    Install-WithWinget -Id 'Rustlang.Rustup' -Label 'Rust (rustup)' -Verify 'rustup'
    # rustup's installer puts ~/.cargo/bin on PATH for future shells only.
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
    & rustup default stable
}

Write-Step 'Checking the MSVC C++ build tools'
if (Test-MsvcBuildTools) {
    Write-Have 'The C++ toolchain Rust links against is present.'
} else {
    Write-Note 'This one is a large download and takes a while.'
    Install-WithWinget -Id 'Microsoft.VisualStudio.2022.BuildTools' `
        -Label 'Visual Studio 2022 build tools (C++)' `
        -ExtraArguments @(
            '--override',
            '--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
        )
    if (-not (Test-MsvcBuildTools)) {
        Write-Note 'The build tools still are not detectable. If the Rust build fails, install the "Desktop development with C++" workload from the Visual Studio Installer.'
    }
}

Write-Step 'Checking the WebView2 runtime'
if (Test-WebView2) {
    Write-Have 'WebView2 is present — that is what renders the app window.'
} else {
    Install-WithWinget -Id 'Microsoft.EdgeWebView2Runtime' -Label 'WebView2 runtime'
}

Write-Step 'Installing project dependencies'
& pnpm install
if ($LASTEXITCODE -ne 0) { throw 'pnpm install failed.' }

if (-not (Test-Have 'ffmpeg')) {
    Write-Note 'No ffmpeg on PATH — the app downloads its own build (~80 MB) the first time it encodes something. Nothing to do.'
}

if ($NoStart) {
    Write-Step 'Ready'
    Write-Host '    Run `pnpm tauri dev` when you want the app.' -ForegroundColor Green
    return
}

Write-Step 'Starting the app (pnpm tauri dev)'
Write-Have 'The first Rust build takes several minutes. Later ones are seconds.'
Write-Have 'Ctrl+C stops it. Edits to the UI reload on save.'
& pnpm tauri dev
