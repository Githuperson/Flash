# Tests both Windows installers on a Windows machine (run from the repository root, after
# scripts/package.sh and scripts/build_windows_installer.ps1). Uses Windows PowerShell 5.1 on
# purpose: that is what most users have. Changes only the current user's PATH, and restores it.
$ErrorActionPreference = 'Stop'
$zip = (Resolve-Path 'dist\flash-windows-x86_64.zip').Path
$setup = (Resolve-Path 'dist\flash-setup-windows-x86_64.exe').Path
$savedPath = [Environment]::GetEnvironmentVariable('Path', 'User')

function Path-Has($dir) { (([Environment]::GetEnvironmentVariable('Path', 'User')) -split ';') -contains $dir }
function Check($ok, $what) { if (-not $ok) { throw "FAILED: $what" } else { Write-Host "ok: $what" } }
function Run-Hello($flash, $dir) {
    $src = Join-Path $dir 'hello.flash'
    Set-Content -Path $src -Value 'say("Hello from the installed Flash")' -Encoding ascii
    & $flash build $src --release -o (Join-Path $dir 'hello.exe') | Out-Null
    $out = (& (Join-Path $dir 'hello.exe')) -join ''
    Check ($out -eq 'Hello from the installed Flash') "a program built by the installed compiler runs ($out)"
}

try {
    # ---- 1. PowerShell installer / uninstaller
    $home1 = Join-Path $env:TEMP 'flash-ps-install-test'
    if (Test-Path $home1) { Remove-Item $home1 -Recurse -Force }
    $env:FLASH_HOME = $home1
    $env:FLASH_ARCHIVE = $zip
    & .\install.ps1
    Remove-Item Env:FLASH_ARCHIVE
    Check (Test-Path "$home1\bin\flash.exe") 'install.ps1 installed flash.exe'
    Check (Path-Has "$home1\bin") 'install.ps1 added bin to the user PATH'
    Check (Test-Path "$home1\stdlib") 'install.ps1 installed the standard library'
    Run-Hello "$home1\bin\flash.exe" $env:TEMP
    & "$home1\uninstall.ps1"
    Check (-not (Test-Path $home1)) 'uninstall.ps1 removed the folder'
    Check (-not (Path-Has "$home1\bin")) 'uninstall.ps1 removed the PATH entry'
    Remove-Item Env:FLASH_HOME

    # ---- 2. setup wizard, silent
    $dir2 = Join-Path $env:TEMP 'flash-setup-test'
    if (Test-Path $dir2) { Remove-Item $dir2 -Recurse -Force }
    $p = Start-Process -FilePath $setup -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=$dir2", '/TASKS=addtopath' -Wait -PassThru
    Check ($p.ExitCode -eq 0) "setup exited with code $($p.ExitCode)"
    Check (Test-Path "$dir2\bin\flash.exe") 'setup installed flash.exe'
    Check (Path-Has "$dir2\bin") 'setup added bin to the user PATH'
    Run-Hello "$dir2\bin\flash.exe" $env:TEMP
    $p = Start-Process -FilePath "$dir2\unins000.exe" -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
    Check ($p.ExitCode -eq 0) 'uninstaller exited with code 0'
    Start-Sleep -Seconds 2
    Check (-not (Test-Path "$dir2\bin\flash.exe")) 'uninstaller removed flash.exe'
    Check (-not (Path-Has "$dir2\bin")) 'uninstaller removed the PATH entry'
    Write-Host 'Both Windows installers work.'
} finally {
    [Environment]::SetEnvironmentVariable('Path', $savedPath, 'User')
}
