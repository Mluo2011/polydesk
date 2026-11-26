@echo off
setlocal
title PolyDirect Windows Installer

set "SRC=%~dp0"
set "APPDIR=%LOCALAPPDATA%\Programs\PolyDirect"
set "STARTMENU=%APPDATA%\Microsoft\Windows\Start Menu\Programs"
set "DESKTOP=%USERPROFILE%\Desktop"

echo ========================================================
echo   Installing PolyDirect (Native Windows 10/11 App)...
echo ========================================================

set "EXE_SRC="
if exist "%SRC%polydirect-windows-x86_64.exe" set "EXE_SRC=%SRC%polydirect-windows-x86_64.exe"
if "%EXE_SRC%"=="" if exist "%SRC%PolyDirect-Windows-x64.exe" set "EXE_SRC=%SRC%PolyDirect-Windows-x64.exe"
if "%EXE_SRC%"=="" if exist "%SRC%PolyDesk-Windows-x64.exe" set "EXE_SRC=%SRC%PolyDesk-Windows-x64.exe"
if "%EXE_SRC%"=="" (
  echo ERROR: polydirect-windows-x86_64.exe not found next to this installer!
  echo Please extract the whole ZIP folder first, then double-click this file.
  pause
  exit /b 1
)

if not exist "%APPDIR%" mkdir "%APPDIR%"

copy /Y "%EXE_SRC%" "%APPDIR%\PolyDirect.exe" >nul
if exist "%SRC%polydesk.ico" copy /Y "%SRC%polydesk.ico" "%APPDIR%\polydesk.ico" >nul
if exist "%SRC%PolyDirect-中文使用指南.html" copy /Y "%SRC%PolyDirect-中文使用指南.html" "%APPDIR%\PolyDirect-中文使用指南.html" >nul

rem Create Uninstaller
(
  echo @echo off
  echo taskkill /IM PolyDirect.exe /F ^>nul 2^>^&1
  echo del /F /Q "%STARTMENU%\PolyDirect.lnk" ^>nul 2^>^&1
  echo del /F /Q "%DESKTOP%\PolyDirect.lnk" ^>nul 2^>^&1
  echo reg delete "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDirect" /f ^>nul 2^>^&1
  echo echo PolyDirect uninstalled. Your local wallet/config in %%APPDATA%%\polydirect (and %%APPDATA%%\polydesk^) was kept safe.
  echo rmdir /S /Q "%APPDIR%"
) > "%APPDIR%\Uninstall-PolyDirect.bat"

rem Create Start Menu + Desktop shortcuts with PolyDirect icon via built-in PowerShell
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ws = New-Object -ComObject WScript.Shell; " ^
  "foreach ($p in @('%STARTMENU%\PolyDirect.lnk', '%DESKTOP%\PolyDirect.lnk')) { " ^
  "  $s = $ws.CreateShortcut($p); " ^
  "  $s.TargetPath = '%APPDIR%\PolyDirect.exe'; " ^
  "  $s.WorkingDirectory = '%APPDIR%'; " ^
  "  $s.Description = 'PolyDirect — Native Polymarket CLOB V2 Trading Terminal'; " ^
  "  if (Test-Path '%APPDIR%\polydesk.ico') { $s.IconLocation = '%APPDIR%\polydesk.ico,0' }; " ^
  "  $s.Save(); " ^
  "}"

rem Register in Windows Settings -> Installed Apps (HKCU, no Admin required)
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDirect" /v "DisplayName" /t REG_SZ /d "PolyDirect" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDirect" /v "DisplayVersion" /t REG_SZ /d "0.3.1" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDirect" /v "Publisher" /t REG_SZ /d "Dronepilot" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDirect" /v "InstallLocation" /t REG_SZ /d "%APPDIR%" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDirect" /v "DisplayIcon" /t REG_SZ /d "%APPDIR%\polydesk.ico" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PolyDirect" /v "UninstallString" /t REG_SZ /d "\"%APPDIR%\Uninstall-PolyDirect.bat\"" /f >nul

echo.
echo Done! PolyDirect is now installed in your Start Menu and Desktop.
echo Launching PolyDirect...
start "" "%APPDIR%\PolyDirect.exe"
exit /b 0
