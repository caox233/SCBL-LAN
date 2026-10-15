@echo off
setlocal DisableDelayedExpansion
pushd "%~dp0" || exit /b 1
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build.ps1" %*
set "result=%errorlevel%"
if not "%result%"=="0" echo BUILD FAILED. Read the error above and artifacts\logs. Do not use an old package as this build's result.
popd
if defined CI goto done
if defined SCBL_NO_PAUSE goto done
pause
:done
exit /b %result%
