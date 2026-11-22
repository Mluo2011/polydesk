@echo off
setlocal
title PolyDesk Windows Installer

set "SRC=%~dp0"
set "APPDIR=%LOCALAPPDATA%\Programs\PolyDesk"
set "STARTMENU=%APPDATA%\Microsoft\Windows\Start Menu\Programs"
set "DESKTOP=%USERPROFILE%\Desktop"

echo ========================================================
echo   Installing PolyDesk (Native Windows 10/11 App)...
echo ========================================================

if not exist "%SRC%PolyDesk-Windows-x64.exe" (
  echo ERROR: PolyDesk-Windows-x64.exe not found next to this installer!
  echo Please extract the whole ZIP folder first, then double-click this file.
  pause
  exit /b 1
)

if not exist "%APPDIR%" mkdir "%APPDIR%"

copy /Y "%SRC%PolyDesk-Windows-x64.exe" "%APPDIR%\PolyDesk.exe" >nul
if exist "%SRC%polydesk.ico" copy /Y "%SRC%polydesk.ico" "%APPDIR%\polydesk.ico" >nul
if exist "%SRC%PolyDesk-中文使用指南.html" copy /Y "%SRC%PolyDesk-中文使用指南.html" "%APPDIR%\PolyDesk-中文使用指南.html" >nul

rem Create Uninstaller
(
  echo @echo off
  echo taskkill /IM PolyDesk.exe /F ^>nul 2^>^&1
  echo del /F /Q "%STARTMENU%\PolyDesk.lnk" ^>nul 2^>^&1
  echo del /F /Q "%DESKTOP%\PolyDesk.lnk" ^>nul 2^>^&1
  echo reg delete "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDesk" /f ^>nul 2^>^&1
  echo echo PolyDesk uninstalled. Your local wallet/config in %%APPDATA%%\polydesk was kept safe.
  echo rmdir /S /Q "%APPDIR%"
) > "%APPDIR%\Uninstall-PolyDesk.bat"

rem Create Start Menu + Desktop shortcuts with PolyDesk icon via built-in PowerShell
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ws = New-Object -ComObject WScript.Shell; " ^
  "foreach ($p in @('%STARTMENU%\PolyDesk.lnk', '%DESKTOP%\PolyDesk.lnk')) { " ^
  "  $s = $ws.CreateShortcut($p); " ^
  "  $s.TargetPath = '%APPDIR%\PolyDesk.exe'; " ^
  "  $s.WorkingDirectory = '%APPDIR%'; " ^
  "  $s.Description = 'PolyDesk — Native Polymarket CLOB V2 Trading Terminal'; " ^
  "  if (Test-Path '%APPDIR%\polydesk.ico') { $s.IconLocation = '%APPDIR%\polydesk.ico,0' }; " ^
  "  $s.Save(); " ^
  "}"

rem Register in Windows Settings -> Installed Apps (HKCU, no Admin required)
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDesk" /v "DisplayName" /t REG_SZ /d "PolyDesk" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDesk" /v "DisplayVersion" /t REG_SZ /d "0.2.0" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDesk" /v "Publisher" /t REG_SZ /d "Dronepilot" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDesk" /v "InstallLocation" /t REG_SZ /d "%APPDIR%" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDesk" /v "DisplayIcon" /t REG_SZ /d "%APPDIR%\polydesk.ico" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDesk" /v "UninstallString" /t REG_SZ /d "\"%APPDIR%\Uninstall-PolyDesk.bat\"" /f >nul

echo.
echo Done! PolyDesk is now installed in your Start Menu and Desktop.
echo Launching PolyDesk...
start "" "%APPDIR%\PolyDesk.exe"
exit /b 0
