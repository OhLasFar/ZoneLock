; Inno Setup script for ZoneLock. Per-user install, no admin, no UAC.
; Build: packaging\package.ps1 (passes the version), or
;        iscc /DAppVersion=1.0.0 packaging\ZoneLock.iss   (Inno Setup 6+)
#ifndef AppVersion
  #define AppVersion "1.0.8"
#endif

[Setup]
; fixed id: upgrades replace the previous install instead of duplicating it
AppId={{5B7E0C2A-9D41-4F3B-A6E2-7C1D8F04B9A3}
AppName=ZoneLock
AppVersion={#AppVersion}
AppVerName=ZoneLock {#AppVersion}
AppPublisher=OhLasFar
AppPublisherURL=https://github.com/OhLasFar/ZoneLock
AppSupportURL=https://github.com/OhLasFar/ZoneLock/issues
AppUpdatesURL=https://github.com/OhLasFar/ZoneLock/releases
LicenseFile=..\LICENSE
DefaultDirName={localappdata}\Programs\ZoneLock
DefaultGroupName=ZoneLock
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=zonelock-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\zonelock.exe
UninstallDisplayName=ZoneLock
; the running daemon holds the exe: close it before replacing files
CloseApplications=force
RestartApplications=no
MinVersion=10.0

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "french"; MessagesFile: "compiler:Languages\French.isl"

[Tasks]
Name: "autostart"; Description: "Start ZoneLock with Windows"; Flags: unchecked

[Files]
Source: "..\target\release\zonelock.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\config.example.json"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\ZoneLock"; Filename: "{app}\zonelock.exe"

[Registry]
; same HKCU Run value the app's own "Start with Windows" toggle uses
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "ZoneLock"; ValueData: """{app}\zonelock.exe"""; Tasks: autostart

[Run]
Filename: "{app}\zonelock.exe"; Description: "Launch ZoneLock"; Flags: nowait postinstall skipifsilent
; silent update started from ZoneLock itself (/update=1): start it again
Filename: "{app}\zonelock.exe"; Parameters: "--updated"; Flags: nowait; Check: IsUpdate

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM zonelock.exe /F"; Flags: runhidden; RunOnceId: "StopZoneLock"

[UninstallDelete]
; browser cache of the settings window; config.json is user data and kept
Type: filesandordirs; Name: "{userappdata}\ZoneLock\WebView2"

[Code]
function IsUpdate: Boolean;
begin
  Result := ExpandConstant('{param:update|0}') = '1';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    // also covers autostart enabled later from inside the app
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'ZoneLock');
end;
