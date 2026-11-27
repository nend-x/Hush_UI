# build.ps1 — one-shot build of the Hush_UI portable exe on Windows.
#
# Usage:
#   .\build.ps1
#
# What it does:
#   1. Verifies the prerequisites (calls .\prereqs-check.ps1).
#   2. Installs npm dependencies (npm install).
#   3. Builds the frontend (npm run build  →  dist\).
#   4. Builds the Rust backend in release mode for x86_64-pc-windows-msvc.
#   5. Prints the location of the resulting flatuihush.exe.
#
# Re-running is safe: every step is idempotent and incremental (cargo will
# reuse cached artifacts from previous runs).

param(
    [switch]$SkipPrereqs  # skip the prereq check (useful for CI)
)

$ErrorActionPreference = "Stop"
$ErrorView = "NormalView"  # don't truncate error output

$root = $PSScriptRoot
Push-Location $root

# Node/npm are sometimes installed but not on PATH (common for per-machine
# installs). Prepend the standard location so `npm install` always resolves.
if (-not (Get-Command npm -ErrorAction SilentlyContinue)) {
    $nodeDir = "$env:ProgramFiles\nodejs"
    if (Test-Path "$nodeDir\npm.cmd") {
        $env:Path = "$nodeDir;$env:Path"
        Write-Host "npm not on PATH - using $nodeDir" -ForegroundColor DarkGray
    }
}

try {
    Write-Host "=== Hush_UI build ===" -ForegroundColor Cyan
    Write-Host "Source tree: $root"
    Write-Host ""

    # --- 1. Prerequisite check (unless -SkipPrereqs) ---
    if (-not $SkipPrereqs) {
        Write-Host "[1/4] Checking prerequisites..." -ForegroundColor Cyan
        & "$root\prereqs-check.ps1"
        if ($LASTEXITCODE -ne 0) {
            Write-Host "Prerequisite check failed. Install the missing tools and re-run." -ForegroundColor Red
            exit 1
        }
        Write-Host ""
    } else {
        Write-Host "[1/4] Prerequisite check skipped (-SkipPrereqs)." -ForegroundColor DarkGray
    }

    # --- 2. npm install ---
    Write-Host "[2/4] Installing npm dependencies..." -ForegroundColor Cyan
    & npm install --no-fund --no-audit
    if ($LASTEXITCODE -ne 0) {
        Write-Host "npm install failed." -ForegroundColor Red
        exit 1
    }
    Write-Host ""

    # --- 3. Build the frontend (Vite → dist\) ---
    Write-Host "[3/4] Building frontend (vite build)..." -ForegroundColor Cyan
    & npm run build
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Frontend build failed." -ForegroundColor Red
        exit 1
    }
    if (-not (Test-Path "$root\dist\index.html") -and
        -not (Test-Path "$root\dist\launcher\index.html")) {
        Write-Host "Frontend build reported success but dist\ is empty. Aborting." -ForegroundColor Red
        exit 1
    }
    Write-Host ""

    # --- 4. Build the Rust backend (release, msvc, x64) ---
    Write-Host "[4/4] Building Rust backend (release, x86_64-pc-windows-msvc)..." -ForegroundColor Cyan
    Push-Location "$root\src-tauri"
    try {
        & cargo build --release --target x86_64-pc-windows-msvc
        if ($LASTEXITCODE -ne 0) {
            Write-Host "Rust build failed." -ForegroundColor Red
            exit 1
        }
    } finally {
        Pop-Location
    }

    $exe = "$root\src-tauri\target\x86_64-pc-windows-msvc\release\hush_ui.exe"
    if (-not (Test-Path $exe)) {
        Write-Host "Build reported success but hush_ui.exe is missing at $exe" -ForegroundColor Red
        exit 1
    }

    $size = (Get-Item $exe).Length / 1MB
    Write-Host ""
    Write-Host "=== Build complete ===" -ForegroundColor Green
    Write-Host ("Output: {0}" -f $exe) -ForegroundColor Green
    Write-Host ("Size:   {0:N2} MB" -f $size) -ForegroundColor Green
    Write-Host ""
    Write-Host "To run: $exe" -ForegroundColor White
    Write-Host "To revert: taskkill /f /im hush_ui.exe" -ForegroundColor DarkGray
    exit 0
} finally {
    Pop-Location
}
