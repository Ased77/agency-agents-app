@echo off
setlocal
cd /d "%~dp0"

where npm >nul 2>nul
if errorlevel 1 (
  echo [ERROR] npm not found on PATH. Install Node.js first.
  pause
  exit /b 1
)

where cargo >nul 2>nul
if errorlevel 1 (
  echo [ERROR] cargo not found on PATH. Install the Rust toolchain first.
  pause
  exit /b 1
)

echo Starting Agency Agents desktop app (tauri dev)...
echo Frontend: http://127.0.0.1:1430
echo Press Ctrl+C in this window to stop.
echo.

call npm run tauri dev
set EXIT_CODE=%ERRORLEVEL%

echo.
echo App exited with code %EXIT_CODE%.
pause
exit /b %EXIT_CODE%