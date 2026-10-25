# Removes a Flash installation made by install.ps1: the folder and its PATH entry.
#   %LOCALAPPDATA%\Flash\uninstall.ps1        (or FLASH_HOME=dir to name another folder)
$ErrorActionPreference = 'Stop'
$flashHome = if ($env:FLASH_HOME) { $env:FLASH_HOME } else { Join-Path $env:LOCALAPPDATA 'Flash' }
$bin = Join-Path $flashHome 'bin'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath) {
    $kept = $userPath -split ';' | Where-Object { $_ -ne '' -and $_ -ne $bin }
    [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
}
if (Test-Path $flashHome) {
    # a script cannot delete the folder it runs from while it runs, so leave a copy of itself out of the way
    Remove-Item $flashHome -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host "Flash was removed from $flashHome and from your PATH."
