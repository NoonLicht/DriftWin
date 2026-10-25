; Inno Setup 6 script for DriftWin. Build: scripts\package.ps1 (passes /DAppVersion=x.y.z and /DSourceDir=...).
#ifndef AppVersion
  #define AppVersion "0.0.1"
#endif
#ifndef SourceDir
  #define SourceDir "..\target\release"
#endif

[Setup]
AppId={{6E1C7B0A-3B8F-4F0E-9B7D-5D2D7C0A9F11}
AppName=DriftWin
AppVersion={#AppVersion}
AppPublisher=David Gadzhimagomedov
AppPublisherURL=https://github.com/NoonLicht/DriftWin
DefaultDirName={autopf}\DriftWin
DefaultGroupName=DriftWin
; per-user install: no administrator rights needed
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=DriftWin-{#AppVersion}-setup
Compression=lzma2
SolidCompression=yes
LicenseFile=..\LICENSE
UninstallDisplayIcon={app}\driftwin.exe
CloseApplications=no

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "autostart"; Description: "Запускать DriftWin при входе в Windows"; Flags: unchecked
Name: "startmenu"; Description: "Ярлык в меню Пуск"

[Files]
Source: "{#SourceDir}\driftwin.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\driftwin-guard.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\driftwin-settings.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\driftwinctl.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\config.example.toml"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\CONFIG.md"; DestDir: "{app}\docs"; Flags: ignoreversion

[Icons]
Name: "{group}\DriftWin"; Filename: "{app}\driftwin.exe"; Parameters: "--onboarding"; Tasks: startmenu; Comment: "Памятка по клавишам"

[Registry]
; the daemon starts in safe mode and needs [windows] only in config.toml (or --all in this command)
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "DriftWin"; \
  ValueData: """{app}\driftwin.exe"""; Tasks: autostart; Flags: uninsdeletevalue

[UninstallRun]
; ask a running daemon to exit: it puts all windows back on screen on the way out
Filename: "{app}\driftwinctl.exe"; Parameters: "msg action quit"; Flags: runhidden; RunOnceId: "StopDriftWin"
