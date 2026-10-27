@echo off
setlocal

REM Rayburst local dev launcher (Windows)
REM Sets Rust / Node22 / pnpm env, then runs tauri dev
REM Double-click this file or run it from cmd.

set "RUSTUP_HOME=D:\tools\rustup"
set "CARGO_HOME=D:\tools\cargo"
set "NODE_DIR=C:\Users\ys101\.workbuddy\binaries\node\versions\22.22.2-6"
set "PATH=%CARGO_HOME%\bin;%NODE_DIR%;%PATH%"

cd /d "D:\workspace-buddy\rayburst"

echo [dev.bat] RUSTUP_HOME = %RUSTUP_HOME%
echo [dev.bat] NODE_DIR    = %NODE_DIR%
node -v
cargo --version
echo.

call pnpm tauri dev

if errorlevel 1 (
  echo [dev.bat] pnpm tauri dev exited with an error.
  pause
)

endlocal
