@echo off
chcp 65001 >nul
cd /d "%~dp0"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
if exist "target\release\mengdownloader.exe" goto run
if exist "target\debug\mengdownloader.exe" goto rundebug
echo Building mengdownloader, the first build takes a while...
cargo build --release
if errorlevel 1 exit /b 1
:run
start "" "%~dp0target\release\mengdownloader.exe"
exit /b 0
:rundebug
start "" "%~dp0target\debug\mengdownloader.exe"
exit /b 0
