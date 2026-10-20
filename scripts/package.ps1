# Builds the Windows deliverables:
#   installer/out/StoryForge_<ver>_x64-setup.exe   NSIS installer (per-user)
#   installer/out/StoryForge-<ver>-portable.zip    folder-free portable build
#
# Requires: Rust (stable), MSVC build tools, and network access on the first run
# so Tauri can download NSIS.

param(
    [switch]$Debug,
    [switch]$SkipBundle
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'installer\out'
New-Item -ItemType Directory -Force -Path $out | Out-Null

Push-Location $root
try {
    if (-not $SkipBundle) {
        $profileArgs = if ($Debug) { @('build', '--debug') } else { @('build') }
        Write-Host "cargo tauri $($profileArgs -join ' ')" -ForegroundColor Cyan
        & cargo tauri @profileArgs
        if ($LASTEXITCODE -ne 0) { throw "tauri build failed with exit code $LASTEXITCODE" }
    }

    $version = (Get-Content (Join-Path $root 'src-tauri\tauri.conf.json') -Raw | ConvertFrom-Json).version
    $profileDir = if ($Debug) { 'debug' } else { 'release' }

    # 1. Installer produced by the NSIS bundler.
    $bundleDir = Join-Path $root "target\$profileDir\bundle\nsis"
    if (Test-Path $bundleDir) {
        Get-ChildItem $bundleDir -Filter '*-setup.exe' | ForEach-Object {
            Copy-Item $_.FullName (Join-Path $out "StoryForge_${version}_x64-setup.exe") -Force
            Write-Host "installer: $($_.Name)" -ForegroundColor Green
        }
    }
    else {
        Write-Warning "no NSIS output at $bundleDir"
    }

    # 2. Portable zip: the exe plus a README. WebView2 is present on Windows 10/11.
    $exe = Join-Path $root "target\$profileDir\storyforge.exe"
    if (-not (Test-Path $exe)) { throw "missing binary: $exe" }
    $staging = Join-Path $env:TEMP "storyforge-portable-$([guid]::NewGuid().ToString('N').Substring(0,8))"
    New-Item -ItemType Directory -Force -Path $staging | Out-Null
    Copy-Item $exe (Join-Path $staging 'StoryForge.exe')
    @"
StoryForge $version — portable build

Run StoryForge.exe. The library database is created on first launch at
%APPDATA%\StoryForge\data\story.db. To use an existing database, set
STORYFORGE_DB to its path, or edit %APPDATA%\StoryForge\config.json.
"@ | Set-Content (Join-Path $staging 'README.txt') -Encoding UTF8
    $zip = Join-Path $out "StoryForge-$version-portable.zip"
    if (Test-Path $zip) { Remove-Item $zip -Force }
    Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $zip
    Remove-Item $staging -Recurse -Force
    Write-Host "portable:  $zip" -ForegroundColor Green
}
finally {
    Pop-Location
}
