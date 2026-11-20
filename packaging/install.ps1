# Per-user installer, no admin rights required.
# Copies zonelock.exe to %LOCALAPPDATA%\Programs\ZoneLock, adds a Start Menu
# shortcut, and optionally enables "Start with Windows" (same HKCU Run
# value the app's own toggle uses, so both stay in sync).
# Usage:
#   powershell -ExecutionPolicy Bypass -File packaging\install.ps1 [-Autostart] [-NoLaunch]
#   powershell -ExecutionPolicy Bypass -File packaging\install.ps1 -Uninstall
param(
    [switch]$Autostart,
    [switch]$NoLaunch,
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $env:LOCALAPPDATA 'Programs\ZoneLock'
$lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\ZoneLock.lnk'
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'

# a running daemon locks the exe
Get-Process zonelock -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 300

if ($Uninstall) {
    Remove-ItemProperty -Path $run -Name 'ZoneLock' -ErrorAction SilentlyContinue
    Remove-Item $lnk -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $dest -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force (Join-Path $env:APPDATA 'ZoneLock\WebView2') -ErrorAction SilentlyContinue
    Write-Host "Uninstalled. Your config is kept in $env:APPDATA\ZoneLock (delete it by hand if you want)."
    return
}

$exe = Join-Path $root 'target\release\zonelock.exe'
if (-not (Test-Path $exe)) {
    Push-Location $root
    cargo build --release
    $code = $LASTEXITCODE
    Pop-Location
    if ($code -ne 0) { throw "cargo build failed" }
}

New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item $exe (Join-Path $dest 'zonelock.exe') -Force
Copy-Item (Join-Path $root 'README.md') $dest -Force -ErrorAction SilentlyContinue

$shell = New-Object -ComObject WScript.Shell
$sc = $shell.CreateShortcut($lnk)
$sc.TargetPath = Join-Path $dest 'zonelock.exe'
$sc.WorkingDirectory = $dest
$sc.Description = 'Keep maximize and F11 inside monitor zones'
$sc.Save()

if (-not $Autostart -and [Environment]::UserInteractive -and $Host.Name -eq 'ConsoleHost' -and -not [Console]::IsInputRedirected) {
    $answer = Read-Host 'Start ZoneLock with Windows? [y/N]'
    $Autostart = $answer -match '^[yY]'
}
if ($Autostart) {
    Set-ItemProperty -Path $run -Name 'ZoneLock' -Value "`"$dest\zonelock.exe`""
    Write-Host 'Autostart enabled (HKCU Run key, per-user).'
}

Write-Host "Installed to $dest"
if (-not $NoLaunch) {
    Start-Process (Join-Path $dest 'zonelock.exe')
    Write-Host 'ZoneLock is running (tray icon). SmartScreen on first run is expected.'
}
