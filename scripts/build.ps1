# Release build of the service (coolrgbd.exe) and control panel (coolrgb.exe) into target\release.
$ErrorActionPreference = 'Stop'
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
Set-Location (Split-Path -Parent $PSScriptRoot)
cargo build --release -p coolrgbd -p coolrgb
if ($LASTEXITCODE -ne 0) { throw 'build failed' }
Get-Item target\release\coolrgbd.exe, target\release\coolrgb.exe | Select-Object Name, Length
