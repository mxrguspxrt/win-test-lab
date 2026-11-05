@echo off
rem win-test-lab: uploaded as C:\wtl-first.cmd and registered under HKLM\...\Run (guest\2003\image.reg),
rem so it runs at the first (automatic) logon of a freshly imported Windows Server 2003 image. It
rem waits for the WTL_BUILD disc on any CD drive and runs its setup.vbs, which finishes the base
rem image and reports BUILD_DONE on COM1. Whatever cscript prints (a script error, for one) goes to
rem C:\wtl-first.log.
set LOG=C:\wtl-first.log
echo %DATE% %TIME% wtl-first.cmd started> %LOG%
set TRIES=0
:again
set FOUND=
for %%D in (D E F G H I J K L M N O P Q R S T U V W X Y Z) do if exist %%D:\WTL\setup.vbs set FOUND=%%D:
if defined FOUND goto :run
set /a TRIES+=1
if %TRIES% geq 36 goto :nodisc
ping -n 6 127.0.0.1 > nul
goto :again
:run
echo %DATE% %TIME% running %FOUND%\WTL\setup.vbs>> %LOG%
cscript.exe //nologo %FOUND%\WTL\setup.vbs >> %LOG% 2>&1
echo %DATE% %TIME% setup.vbs exit=%ERRORLEVEL%>> %LOG%
goto :eof
:nodisc
echo %DATE% %TIME% the WTL_BUILD disc was not found>> %LOG%
echo WTL BUILD_FAIL ^| the WTL_BUILD disc was not found on any drive> \\.\COM1
