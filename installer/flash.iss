; Inno Setup script for the Windows installer (built by .github/workflows/release.yml).
;   iscc /DAppVersion=0.2.0 /DStageDir=<unpacked flash-windows-x86_64 archive> /DOutDir=<dir> installer\flash.iss
; Installs per user (no administrator rights needed) and adds <install dir>\bin to the user's PATH.
; The installer is not code-signed, so Windows SmartScreen may show a warning the first time.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef StageDir
  #define StageDir "..\stage"
#endif
#ifndef OutDir
  #define OutDir "..\dist"
#endif

[Setup]
AppId={{D7F3A573-375A-4438-843B-DECF9B4A6D56}
AppName=Flash
AppVersion={#AppVersion}
AppVerName=Flash {#AppVersion}
AppPublisher=Githuperson
AppPublisherURL=https://github.com/Githuperson/Flash
AppSupportURL=https://github.com/Githuperson/Flash/issues
DefaultDirName={localappdata}\Programs\Flash
DefaultGroupName=Flash
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutDir}
OutputBaseFilename=flash-setup-windows-x86_64
Compression=lzma2
SolidCompression=yes
ChangesEnvironment=yes
WizardStyle=modern
UninstallDisplayName=Flash {#AppVersion}
LicenseFile={#StageDir}\LICENSE

[Tasks]
Name: "addtopath"; Description: "Add Flash to my PATH (so that the 'flash' command works in any terminal)"; GroupDescription: "Setup:"

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: recursesubdirs createallsubdirs ignoreversion

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}\bin"; Check: NeedsAddPath(ExpandConstant('{app}\bin')); Tasks: addtopath

[Run]
Filename: "{cmd}"; Parameters: "/k ""{app}\bin\flash.exe"" --version && echo. && echo Flash is installed. Try:  flash run hello.flash"; Description: "Open a terminal and check the installation"; Flags: postinstall nowait skipifsilent unchecked

[Code]
function NeedsAddPath(Dir: string): Boolean;
var
  Path: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Path) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') = 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Path, Dir, Upper: string;
  P: Integer;
begin
  if CurUninstallStep <> usPostUninstall then
    exit;
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Path) then
    exit;
  Dir := ExpandConstant('{app}\bin');
  Upper := Uppercase(Dir);
  { remove the entry together with one neighbouring separator }
  P := Pos(';' + Upper, Uppercase(Path));
  if P > 0 then
    Delete(Path, P, Length(Dir) + 1)
  else
  begin
    P := Pos(Upper + ';', Uppercase(Path));
    if P = 1 then
      Delete(Path, 1, Length(Dir) + 1)
    else if Uppercase(Path) = Upper then
      Path := '';
  end;
  RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Path);
end;
