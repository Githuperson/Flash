# Builds dist\flash-setup-windows-x86_64.exe (an Inno Setup wizard) from dist\flash-windows-x86_64.zip.
# Run from the repository root on Windows after scripts/package.sh windows-x86_64.
param([string]$Zip = 'dist\flash-windows-x86_64.zip')
$ErrorActionPreference = 'Stop'
$stage = Join-Path ([IO.Path]::GetTempPath()) 'flash-installer-stage'
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
Expand-Archive -Path $Zip -DestinationPath $stage -Force
$version = ((& (Join-Path $stage 'bin\flash.exe') --version) -split ' ')[1]
$iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
if (-not (Test-Path $iscc)) {
    choco install innosetup -y --no-progress | Out-Host
}
if (-not (Test-Path $iscc)) { throw "Inno Setup compiler not found at $iscc" }
$out = (Resolve-Path (Split-Path $Zip -Parent)).Path
& $iscc "/DAppVersion=$version" "/DStageDir=$stage" "/DOutDir=$out" 'installer\flash.iss'
if ($LASTEXITCODE -ne 0) { throw "ISCC failed with exit code $LASTEXITCODE" }
$exe = Join-Path $out 'flash-setup-windows-x86_64.exe'
$hash = (Get-FileHash -Algorithm SHA256 $exe).Hash.ToLower()
Set-Content -Path "$exe.sha256" -Value "$hash  flash-setup-windows-x86_64.exe" -Encoding ascii
Get-Item $exe, "$exe.sha256" | Format-Table Name, Length
