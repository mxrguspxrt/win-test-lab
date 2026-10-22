@echo off
rem win-test-lab demo "program": starts the PowerShell echo server (TCP 5000).
rem Stand-in for the real program.exe. Extra args are passed through.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0echo-server.ps1" %*
