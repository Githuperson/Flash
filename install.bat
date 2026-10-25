@echo off
rem Double-click to install Flash for the current user and add it to your PATH (no administrator needed).
rem Works from the unpacked flash-windows-x86_64 folder, or on its own: then it downloads the
rem latest release first.
setlocal
if exist "%~dp0install.ps1" (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1"
) else (
    powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://github.com/Githuperson/Flash/releases/latest/download/install.ps1 | iex"
)
echo.
pause
