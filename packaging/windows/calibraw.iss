#preproc ispp

#ifndef AppVersion
  #error AppVersion must be supplied by the build workflow
#endif

#ifndef ArtifactArch
  #error ArtifactArch must be supplied by the build workflow
#endif

#define PackageDir "..\..\dist\CalibRaw-windows-" + ArtifactArch

[Setup]
AppId={{BC6F251A-F9D8-47A4-B1D4-912C13082DD0}
AppName=CalibRaw
AppVersion={#AppVersion}
AppVerName=CalibRaw {#AppVersion}
AppPublisher=Duecki and CalibRaw contributors
AppPublisherURL=https://github.com/Duecki1/CalibRaw
AppSupportURL=https://github.com/Duecki1/CalibRaw/issues
AppUpdatesURL=https://github.com/Duecki1/CalibRaw/releases
AppReadmeFile={app}\README.md
DefaultDirName={autopf}\CalibRaw
DefaultGroupName=CalibRaw
DisableProgramGroupPage=yes
LicenseFile={#PackageDir}\COPYING
OutputDir=..\..\dist
OutputBaseFilename=CalibRaw-windows-{#ArtifactArch}-setup
SetupIconFile=..\icons\calibraw.ico
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=admin
Uninstallable=yes
CreateUninstallRegKey=yes
UninstallDisplayName=CalibRaw
UninstallDisplayIcon={app}\calibraw.exe
#if ArtifactArch == "x86_64"
ArchitecturesAllowed=x64compatible and not arm64
ArchitecturesInstallIn64BitMode=x64compatible
#elif ArtifactArch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
  #error Unsupported ArtifactArch
#endif

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#PackageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\CalibRaw"; Filename: "{app}\calibraw.exe"; WorkingDir: "{app}"
Name: "{group}\{cm:UninstallProgram,CalibRaw}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\CalibRaw"; Filename: "{app}\calibraw.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\calibraw.exe"; Description: "{cm:LaunchProgram,CalibRaw}"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent
