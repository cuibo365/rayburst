@echo off
rem Clean the fast-growing parts of target\debug:
rem   - incremental cache (the largest item, grows every dev session)
rem   - leftover .pdb files (not produced anymore since debug = false)
rem Compiled libs (.rlib/.rmeta) are KEPT, so the next dev start stays fast.
setlocal
set "DBG=%~dp0src-tauri\target\debug"

echo [clean-cache] target: %DBG%

if not exist "%DBG%" (
  echo [clean-cache] Nothing to clean, no debug directory.
  exit /b 0
)

if exist "%DBG%\incremental" (
  for /f %%s in ('dir /s /q "%DBG%\incremental" ^| find "File(s)"') do echo [clean-cache] incremental size: %%s
  echo [clean-cache] Removing incremental cache ...
  rmdir /s /q "%DBG%\incremental"
) else (
  echo [clean-cache] No incremental cache found.
)

if exist "%DBG%\*.pdb" (
  echo [clean-cache] Removing leftover .pdb files ...
  del /q "%DBG%\*.pdb" 2>nul
  del /q "%DBG%\deps\*.pdb" 2>nul
)

echo [clean-cache] Done. Next "dev.bat" start rebuilds a fresh incremental cache.
pause
