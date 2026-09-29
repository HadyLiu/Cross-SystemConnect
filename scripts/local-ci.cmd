@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0local-ci.ps1" %*
exit /b %ERRORLEVEL%
