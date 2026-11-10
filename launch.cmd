@echo off
rem Builds Restrafe, finds your STRAFTAT installation the first time, puts Restrafe.exe in this folder and starts it.
rem After that you can start the game by double-clicking Restrafe.exe (no command prompt needed).
setlocal
cd /d "%~dp0game" || exit /b 1
cargo build --release --bins --locked || exit /b 1
if not exist "%LOCALAPPDATA%\restrafe\installation.json" (
    target\release\setup.exe --no-pause || exit /b 1
)
copy /y target\release\restrafe.exe "%~dp0Restrafe.exe" >nul || exit /b 1
start "" "%~dp0Restrafe.exe" %*
