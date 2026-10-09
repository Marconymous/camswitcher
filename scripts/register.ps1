#Requires -RunAsAdministrator
<#
.SYNOPSIS
  Installs and registers the CamSwitch virtual camera DLL (or removes it).

.DESCRIPTION
  Copies vcam_source.dll to "C:\Program Files\CamSwitch" (readable by the
  Frame Server service, which runs as LocalService) and registers its COM
  class under HKLM. Stops Frame Server first so a previously loaded DLL is
  released. Re-run after every DLL rebuild.

.EXAMPLE
  .\scripts\register.ps1              # debug build
  .\scripts\register.ps1 -Release     # release build
  .\scripts\register.ps1 -Unregister
#>
param(
    [switch]$Release,
    [switch]$Unregister
)

$ErrorActionPreference = 'Stop'
$profileDir = if ($Release) { 'release' } else { 'debug' }
$src = Join-Path $PSScriptRoot "..\target\$profileDir\vcam_source.dll"
$dir = Join-Path $env:ProgramFiles 'CamSwitch'
$dst = Join-Path $dir 'vcam_source.dll'

function Invoke-Regsvr32([string[]]$argv) {
    $p = Start-Process regsvr32.exe -ArgumentList $argv -Wait -PassThru -WindowStyle Hidden
    if ($p.ExitCode -ne 0) { throw "regsvr32 $($argv -join ' ') failed with exit code $($p.ExitCode)" }
}

# Release the DLL if Frame Server has it loaded.
Stop-Service FrameServer, FrameServerMonitor -Force -ErrorAction SilentlyContinue

if ($Unregister) {
    if (Test-Path $dst) {
        Invoke-Regsvr32 @('/s', '/u', "`"$dst`"")
        Remove-Item $dst -Force
    }
    Write-Host 'CamSwitch virtual camera unregistered.'
    return
}

if (-not (Test-Path $src)) { throw "DLL not found: $src (run cargo build first)" }
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item $src $dst -Force
Invoke-Regsvr32 @('/s', "`"$dst`"")
Write-Host "Registered $dst"
