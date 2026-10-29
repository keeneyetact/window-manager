# Installs Window Manager for the current user and starts it with Windows.
# It runs in the background with only a notification-area (tray) icon.
#
#   powershell -ExecutionPolicy Bypass -File install.ps1              # install
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Uninstall   # remove

param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'
$name = 'WindowManager'
$dir = Join-Path $env:LOCALAPPDATA "Programs\$name"
$exe = Join-Path $dir 'windows-manager.exe'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'

Get-Process windows-manager -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

if ($Uninstall) {
    Remove-ItemProperty -Path $runKey -Name $name -ErrorAction SilentlyContinue
    Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "Window Manager uninstalled."
    return
}

Push-Location $PSScriptRoot
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally {
    Pop-Location
}

New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item (Join-Path $PSScriptRoot 'target\release\windows-manager.exe') $exe -Force

Set-ItemProperty -Path $runKey -Name $name -Value "`"$exe`" --background"
Start-Process $exe -ArgumentList '--background'

Write-Host "Installed to $exe"
Write-Host "It starts with Windows. Open it from the tray icon (under ^ hidden icons)."
