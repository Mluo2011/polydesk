#ifndef MyAppVersion
  #define MyAppVersion "0.3.2"
#endif

[Setup]
AppId={{B7F4E2A9-4C81-4D2E-9F1A-8E5D3B6C2A01}
AppName=PolyDirect
AppVersion={#MyAppVersion}
AppPublisher=Dronepilot
AppPublisherURL=https://github.com/Mluo2011/polydirect
AppSupportURL=https://github.com/Mluo2011/polydirect/releases
DefaultDirName={localappdata}\Programs\PolyDirect
DefaultGroupName=PolyDirect
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\release-artifacts
OutputBaseFilename=PolyDirect-Setup
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
Source: "..\target\x86_64-pc-windows-msvc\release\polydesk.exe"; DestDir: "{app}"; DestName: "PolyDirect.exe"; Flags: ignoreversion
Source: "..\assets\polydesk.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "PolyDirect-中文使用指南.html"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\PolyDirect\PolyDirect"; Filename: "{app}\PolyDirect.exe"; IconFilename: "{app}\polydesk.ico"
Name: "{autoprograms}\PolyDirect\PolyDirect 中文使用指南"; Filename: "{app}\PolyDirect-中文使用指南.html"
Name: "{autodesktop}\PolyDirect"; Filename: "{app}\PolyDirect.exe"; IconFilename: "{app}\polydesk.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\PolyDirect.exe"; Description: "Launch PolyDirect now"; Flags: nowait postinstall skipifsilent
