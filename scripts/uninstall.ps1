# Removes Cool'n RGB. Devices go back to their stored hardware lighting profiles.
#Requires -RunAsAdministrator
param([switch]$KeepSettings)
$dest = Join-Path $env:ProgramFiles 'CoolRGB'

try {
    $pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'coolrgb', [System.IO.Pipes.PipeDirection]::InOut)
    $pipe.Connect(1000)
    $w = New-Object System.IO.StreamWriter($pipe)
    $w.WriteLine('{"op":"quit"}'); $w.Flush()
    (New-Object System.IO.StreamReader($pipe)).ReadLine() | Out-Null
    $pipe.Dispose()
} catch { }
Get-Process coolrgbd -ErrorAction SilentlyContinue | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
Get-Process coolrgbd, coolrgb -ErrorAction SilentlyContinue | Stop-Process -Force

schtasks.exe /Delete /F /TN "Cool'n RGB" 2>$null | Out-Null
Remove-Item -Recurse -Force $dest -ErrorAction SilentlyContinue
Remove-Item (Join-Path ([Environment]::GetFolderPath('CommonPrograms')) "Cool'n RGB.lnk") -ErrorAction SilentlyContinue
if (-not $KeepSettings) { Remove-Item -Recurse -Force (Join-Path $env:APPDATA 'coolrgb') -ErrorAction SilentlyContinue }
Write-Host 'Cool''n RGB removed.'
