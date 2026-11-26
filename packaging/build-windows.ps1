$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot

Write-Host "Building PolyDirect native Windows 10/11 GPU app (ZERO browser / ZERO WebView)..." -ForegroundColor Cyan
Push-Location $Root
cargo build --release --bin polydesk
Pop-Location

Write-Host "Done! Native standalone app ready at: $Root\target\release\polydesk.exe" -ForegroundColor Green
