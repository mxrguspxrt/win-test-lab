@echo off
rem Example admin step: "install" the app from the mounted drive (X:) to C:\App as the SYSTEM account.
echo [setup.bat] running as %USERDOMAIN%\%USERNAME%
if not exist C:\App mkdir C:\App
copy /y "%~dp0smoke.exe" C:\App\ >nul
echo [setup.bat] installed smoke.exe into C:\App
exit /b 0
