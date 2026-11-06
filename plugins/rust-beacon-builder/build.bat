@echo off
setlocal
set "PLUGIN_ID=rust-beacon-builder"
set "OUT=%~dp0%PLUGIN_ID%.zip"
if exist "%OUT%" del /q "%OUT%"
powershell -NoProfile -ExecutionPolicy Bypass -Command "Compress-Archive -Path '%~dp0config.json','%~dp0rust-beacon-builder.html','%~dp0rust-beacon-builder.css','%~dp0rust-beacon-builder.js','%~dp0server.js' -DestinationPath '%OUT%' -Force"
if errorlevel 1 exit /b 1
echo [ok] %OUT%
