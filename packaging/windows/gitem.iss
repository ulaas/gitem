; Inno Setup script for gitem's Windows installer. Built by packaging/windows/build.ps1, which passes
; the version, so it's kept in Cargo.toml alone:
;
;   iscc /DAppVersion=0.5.2 /DSourceExe=path\to\gitem.exe packaging\windows\gitem.iss
;
; It installs for the current user only and asks for no administrator rights. It writes under
; %LOCALAPPDATA%\Programs\gitem, the user's Start Menu and HKCU. The Explorer entry and the PATH
; addition are optional tasks, off by default. An uninstall takes all of it back. It doesn't touch
; settings or stored credentials, which belong to the user.
;
; The installer isn't signed: that needs the owner's certificate, and SmartScreen warns until then.

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z
#endif
#ifndef SourceExe
  #error Pass /DSourceExe=path\to\gitem.exe
#endif

[Setup]
AppId={{6E3B0B58-6E0C-4C5E-9E0F-5F6B7A3D2C11}
AppName=gitem
AppVersion={#AppVersion}
AppPublisher=gitem
DefaultDirName={localappdata}\Programs\gitem
DefaultGroupName=gitem
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ChangesEnvironment=yes
ChangesAssociations=yes
OutputBaseFilename=gitem-{#AppVersion}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\gitem.exe
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Tasks]
Name: "path"; Description: "Add gitem to PATH, so that ""gitem ."" works in a terminal"; Flags: unchecked
Name: "explorer"; Description: "Add ""Open in gitem"" to folders in Explorer"; Flags: unchecked
Name: "links"; Description: "Open gitem:// links in gitem"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "gitem.exe"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\gitem"; Filename: "{app}\gitem.exe"

[Registry]
; The same entries as Settings > Agents and integrations writes: see shell_integration in app code.
Root: HKCU; Subkey: "Software\Classes\Directory\shell\gitem"; ValueType: string; ValueData: "Open in gitem"; Tasks: explorer; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\shell\gitem"; ValueName: "Icon"; ValueType: string; ValueData: "{app}\gitem.exe"; Tasks: explorer
Root: HKCU; Subkey: "Software\Classes\Directory\shell\gitem\command"; ValueType: string; ValueData: """{app}\gitem.exe"" ""%1"""; Tasks: explorer
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\gitem"; ValueType: string; ValueData: "Open in gitem"; Tasks: explorer; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\gitem"; ValueName: "Icon"; ValueType: string; ValueData: "{app}\gitem.exe"; Tasks: explorer
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\gitem\command"; ValueType: string; ValueData: """{app}\gitem.exe"" ""%V"""; Tasks: explorer
Root: HKCU; Subkey: "Software\Classes\gitem"; ValueType: string; ValueData: "URL:gitem"; Tasks: links; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\gitem"; ValueName: "URL Protocol"; ValueType: string; ValueData: ""; Tasks: links
Root: HKCU; Subkey: "Software\Classes\gitem\shell\open\command"; ValueType: string; ValueData: """{app}\gitem.exe"" ""%1"""; Tasks: links

[Run]
Filename: "{app}\gitem.exe"; Description: "Start gitem"; Flags: nowait postinstall skipifsilent

[Code]
const EnvKey = 'Environment';

function PathHas(const Dir: string): Boolean;
var Current: string;
begin
  Result := RegQueryStringValue(HKCU, EnvKey, 'Path', Current) and (Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Current) + ';') > 0);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var Current, Dir: string;
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('path') then begin
    Dir := ExpandConstant('{app}');
    if not PathHas(Dir) then begin
      if not RegQueryStringValue(HKCU, EnvKey, 'Path', Current) then Current := '';
      if (Current <> '') and (Copy(Current, Length(Current), 1) <> ';') then Current := Current + ';';
      RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Current + Dir);
    end;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var Current, Dir: string; At: Integer;
begin
  if CurUninstallStep = usPostUninstall then begin
    Dir := ExpandConstant('{app}');
    if RegQueryStringValue(HKCU, EnvKey, 'Path', Current) then begin
      At := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Current) + ';');
      if At > 0 then begin
        Delete(Current, At, Length(Dir) + 1);
        if (Current <> '') and (Copy(Current, Length(Current), 1) = ';') then Delete(Current, Length(Current), 1);
        RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Current);
      end;
    end;
  end;
end;
