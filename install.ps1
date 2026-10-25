# Flash installer for Windows (PowerShell). Installs for the current user, no administrator needed.
#
#   irm https://raw.githubusercontent.com/Githuperson/Flash/main/install.ps1 | iex
#
# Downloads flash-windows-x86_64.zip from the latest GitHub release, checks its SHA-256, unpacks it
# into %LOCALAPPDATA%\Flash and adds its bin folder to your PATH. Open a NEW terminal afterwards.
# Options (environment variables): FLASH_HOME=dir   FLASH_REPO=url   FLASH_RELEASE_URL=base url
#   FLASH_ARCHIVE=path to a local flash-windows-x86_64.zip (skips the download)
# Run from an unpacked archive (double-click install.bat), it installs that copy.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch {}

$repo = if ($env:FLASH_REPO) { $env:FLASH_REPO } else { 'https://github.com/Githuperson/Flash' }
$base = if ($env:FLASH_RELEASE_URL) { $env:FLASH_RELEASE_URL } else { "$repo/releases/latest/download" }
$flashHome = if ($env:FLASH_HOME) { $env:FLASH_HOME } else { Join-Path $env:LOCALAPPDATA 'Flash' }
$name = 'flash-windows-x86_64.zip'
$tmp = Join-Path ([IO.Path]::GetTempPath()) ('flash-install-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    $source = $null   # a folder containing bin\, stdlib\, include\
    $here = if ($PSScriptRoot) { $PSScriptRoot } else { $null }
    if (-not $env:FLASH_ARCHIVE -and $here -and (Test-Path (Join-Path $here 'bin\flash.exe'))) {
        $source = $here
        Write-Host "Installing the copy of Flash in $here"
    } else {
        if ($env:FLASH_ARCHIVE) {
            $zip = $env:FLASH_ARCHIVE
        } else {
            $zip = Join-Path $tmp $name
            Write-Host "Downloading $base/$name"
            Invoke-WebRequest -Uri "$base/$name" -OutFile $zip -UseBasicParsing
            $sumFile = Join-Path $tmp "$name.sha256"
            Invoke-WebRequest -Uri "$base/$name.sha256" -OutFile $sumFile -UseBasicParsing
            $want = ((Get-Content $sumFile -Raw).Trim() -split '\s+')[0].ToLower()
            $got = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
            if ($want -ne $got) { throw "Checksum mismatch for the downloaded archive; refusing to install." }
        }
        $source = Join-Path $tmp 'unpacked'
        Expand-Archive -Path $zip -DestinationPath $source -Force
    }

    New-Item -ItemType Directory -Force -Path $flashHome | Out-Null
    foreach ($d in 'bin', 'lib', 'stdlib', 'include') {
        $p = Join-Path $flashHome $d
        if (Test-Path $p) { Remove-Item $p -Recurse -Force }
    }
    foreach ($d in 'bin', 'stdlib', 'include') {
        $from = Join-Path $source $d
        if (Test-Path $from) { Copy-Item $from -Destination $flashHome -Recurse -Force }
    }
    if (Test-Path (Join-Path $source 'LICENSE')) { Copy-Item (Join-Path $source 'LICENSE') -Destination $flashHome -Force }
    $uninstall = Join-Path $source 'uninstall.ps1'
    if (Test-Path $uninstall) { Copy-Item $uninstall -Destination $flashHome -Force }

    # Put bin on the user's PATH (this also tells running programs that the environment changed).
    $bin = Join-Path $flashHome 'bin'
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $userPath) { $userPath = '' }
    $entries = $userPath -split ';' | Where-Object { $_ -ne '' }
    if ($entries -notcontains $bin) {
        [Environment]::SetEnvironmentVariable('Path', (($entries + $bin) -join ';'), 'User')
        Write-Host "Added $bin to your PATH."
    }
    if (($env:Path -split ';') -notcontains $bin) { $env:Path = "$env:Path;$bin" }

    Write-Host ''
    Write-Host "Flash is installed in $flashHome"
    & (Join-Path $bin 'flash.exe') --version
    Write-Host ''
    Write-Host 'Open a NEW terminal window, then try:'
    Write-Host '    echo say("Hello, world!") > hello.flash'
    Write-Host '    flash run hello.flash'
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
