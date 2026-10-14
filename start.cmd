@echo off
rem CallCenter starten: Datenbank + Server, Browser öffnet sich, sobald der Server bereit ist.
rem Fenster offen lassen, solange die Software laufen soll (Strg+C beendet sie).
title CallCenter
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run.ps1" -Open
pause
