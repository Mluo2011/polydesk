#ifndef MyAppVersion
  #define MyAppVersion "0.2.1"
#endif

[Setup]
AppId={{B7F4E2A9-4C81-4D2E-9F1A-8E5D3B6C2A01}
AppName=PolyDesk
AppVersion={#MyAppVersion}
AppPublisher=Dronepilot
AppPublisherURL=https://github.com/Mluo2011/polydesk
AppSupportURL=https://github.com/Mluo2011/polydesk/releases
DefaultDirName={localappdata}\Programs\PolyDesk
DefaultGroupName=PolyDesk
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\release-artifacts
OutputBaseFilename=PolyDesk-Setup
SetupIconFile=..\assets\polydesk.ico
UninstallDisplayIcon={app}\polydesk.ico
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &Desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Files]
Source: "..\target\x86_64-pc-windows-msvc\release\polydesk.exe"; DestDir: "{app}"; DestName: "PolyDesk.exe"; Flags: ignoreversion
Source: "..\assets\polydesk.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "PolyDesk-中文使用指南.html"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\PolyDesk\PolyDesk"; Filename: "{app}\PolyDesk.exe"; IconFilename: "{app}\polydesk.ico"
Name: "{autoprograms}\PolyDesk\PolyDesk 中文使用指南"; Filename: "{app}\PolyDesk-中文使用指南.html"
Name: "{autodesktop}\PolyDesk"; Filename: "{app}\PolyDesk.exe"; IconFilename: "{app}\polydesk.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\PolyDesk.exe"; Description: "Launch PolyDesk now"; Flags: nowait postinstall skipifsilent
