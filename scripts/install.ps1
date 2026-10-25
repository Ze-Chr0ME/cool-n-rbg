# Installs Cool'n RGB to "C:\Program Files\CoolRGB" and starts the background service.
# Run from an elevated PowerShell:  powershell -ExecutionPolicy Bypass -File scripts\install.ps1
#Requires -RunAsAdministrator
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$src = Join-Path $root 'target\release'
$dest = Join-Path $env:ProgramFiles 'CoolRGB'

foreach ($exe in 'coolrgbd.exe', 'coolrgb.exe') {
    if (-not (Test-Path (Join-Path $src $exe))) { throw "Missing $src\$exe - run scripts\build.ps1 first." }
}

# Ask a running service to exit without switching the lights back to their hardware profiles.
function Stop-Service-Handover {
    try {
        $pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'coolrgb', [System.IO.Pipes.PipeDirection]::InOut)
        $pipe.Connect(1000)
        $w = New-Object System.IO.StreamWriter($pipe)
        $w.WriteLine('{"op":"handover"}'); $w.Flush()
        (New-Object System.IO.StreamReader($pipe)).ReadLine() | Out-Null
        $pipe.Dispose()
    } catch { }
    Get-Process coolrgbd -ErrorAction SilentlyContinue | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
    Get-Process coolrgbd, coolrgb -ErrorAction SilentlyContinue | Stop-Process -Force
    Get-Process coolrgbd, coolrgb -ErrorAction SilentlyContinue | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
}

Stop-Service-Handover
New-Item -ItemType Directory -Force $dest | Out-Null
Get-ChildItem $dest -Filter '*.old.exe' -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
foreach ($exe in 'coolrgbd.exe', 'coolrgb.exe') {
    $target = Join-Path $dest $exe
    # A file that is still locked (window closing, antivirus scan) can be renamed but not overwritten.
    try { Copy-Item (Join-Path $src $exe) $target -Force }
    catch {
        Rename-Item $target ([IO.Path]::ChangeExtension($exe, '.old.exe')) -Force
        Copy-Item (Join-Path $src $exe) $target -Force
    }
}

$lnk = Join-Path ([Environment]::GetFolderPath('CommonPrograms')) "Cool'n RGB.lnk"
$sh = New-Object -ComObject WScript.Shell
$s = $sh.CreateShortcut($lnk)
$s.TargetPath = Join-Path $dest 'coolrgb.exe'
$s.WorkingDirectory = $dest
$s.Save()

Start-Process (Join-Path $dest 'coolrgbd.exe')
Write-Host "Installed to $dest. Open Cool'n RGB from the Start menu; enable 'Start with Windows' under Settings."
