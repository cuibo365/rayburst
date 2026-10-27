@echo off
setlocal

REM Rayburst local packaging script (Windows)
REM Sets the Rust / Node22 / pnpm environment, then runs "pnpm tauri build".
REM
REM Usage:
REM   build.bat            Build NSIS installer (same bundle type as CI)
REM   build.bat msi        Build MSI installer
REM   build.bat all        Build both MSI and NSIS
REM
REM Output:
REM   src-tauri\target\x86_64-pc-windows-msvc\release\bundle\
REM
REM Note: "beforeBuildCommand" in tauri.conf.json automatically runs
REM       "pnpm build:native-launcher" and "pnpm build" first.

set "RUSTUP_HOME=D:\tools\rustup"
set "CARGO_HOME=D:\tools\cargo"
set "NODE_DIR=C:\Users\ys101\.workbuddy\binaries\node\versions\22.22.2-6"
set "PATH=%CARGO_HOME%\bin;%NODE_DIR%;%PATH%"

set "BUNDLES=%~1"
if "%BUNDLES%"=="" set "BUNDLES=nsis"

cd /d "D:\workspace-buddy\rayburst"

echo [build.bat] RUSTUP_HOME = %RUSTUP_HOME%
echo [build.bat] NODE_DIR    = %NODE_DIR%
echo [build.bat] BUNDLES     = %BUNDLES%
node -v
cargo --version
echo.
echo [build.bat] Starting release build (first run can take a long time)...
echo.

call pnpm tauri build --target x86_64-pc-windows-msvc --bundles %BUNDLES%

if errorlevel 1 (
  echo.
  echo [build.bat] BUILD FAILED. See the output above.
  pause
  exit /b 1
)

echo.
echo [build.bat] BUILD OK.
echo [build.bat] Installer(s):
dir /b /s "src-tauri\target\x86_64-pc-windows-msvc\release\bundle\*.exe" 2>nul
dir /b /s "src-tauri\target\x86_64-pc-windows-msvc\release\bundle\*.msi" 2>nul
echo.

pause
endlocal
