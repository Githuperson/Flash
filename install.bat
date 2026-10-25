@echo off
rem Double-click to install Flash for the current user and add it to your PATH.
rem (Run this from the unpacked flash-windows-x86_64 folder.)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1"
echo.
pause
