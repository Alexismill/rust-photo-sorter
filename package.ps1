# Builds the release executable and wraps it in a ZIP ready to send.
#
# Run from the project folder:
#   .\package.ps1
#
# If PowerShell refuses to run it ("execution of scripts is disabled"), either
# allow scripts for this session only:
#   Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass
# or run the three cargo/Compress-Archive lines by hand.

$ErrorActionPreference = "Stop"

# Version is read from Cargo.toml so the ZIP name never drifts from the build.
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$name    = "photo-sorter-$version-windows-x64"
$staging = "dist\$name"

Write-Host "Building $name (this takes a few minutes with LTO enabled)..."
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "build failed" }

# Start from a clean staging folder, or a stale file from a previous run ends
# up in the ZIP.
if (Test-Path $staging) { Remove-Item $staging -Recurse -Force }
New-Item -ItemType Directory -Path $staging -Force | Out-Null

Copy-Item "target\release\photo-sorter.exe" $staging
Copy-Item "README.md"  $staging
Copy-Item "LICENSE"    $staging

$zip = "dist\$name.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path "$staging\*" -DestinationPath $zip

$sizeMb = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host ""
Write-Host "Done: $zip ($sizeMb MB)" -ForegroundColor Green
Write-Host "Self-contained. The recipient extracts it and runs photo-sorter.exe."
