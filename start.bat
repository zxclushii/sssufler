@echo off
setlocal
cd /d "%~dp0"
set "EXE=%~dp0node_modules\electron\dist\electron.exe"
where node 1>NUL 2>NUL
if errorlevel 1 (
  echo Node.js not found. Install it from https://nodejs.org and run this file again.
  pause
  exit /b 1
)
if not exist "node_modules\electron\install.js" (
  echo Installing dependencies, wait a minute...
  call npm install --no-audit --no-fund
  if errorlevel 1 goto fail
)
if not exist "%EXE%" (
  echo Downloading Electron, about 100 MB...
  node "node_modules\electron\install.js"
)
if not exist "%EXE%" goto fail
start "" "%EXE%" "%~dp0."
exit /b 0
:fail
echo.
echo Something went wrong. Send a screenshot of this window.
pause
exit /b 1
