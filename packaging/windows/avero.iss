; Inno Setup 6 script for the Avero installer.
;
; Built by the CI (.github/workflows/release.yml); by hand, from the
; repository root, after `cargo build --release -p avero-app`:
;
;   iscc /DAppVersion=0.1.0 packaging\windows\avero.iss
;
; Installs for the current user only (no administrator rights), in
; %LOCALAPPDATA%\Programs\Avero. The user's mail and settings
; (%APPDATA%\Avero) and the passwords in the Windows keyring are left
; in place by the uninstaller.

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
; The executable built by Cargo, relative to this file.
#ifndef SourceExe
  #define SourceExe "..\..\target\release\avero-app.exe"
#endif
; Must stay equal to APP_USER_MODEL_ID in crates/avero-app/src/notify.rs:
; Windows shows the notifications of that id under this shortcut's name
; and icon.
#define AppUserModelID "Avero.Avero"

[Setup]
; Identifies Avero for upgrades and the uninstaller: never change it.
AppId={{3F76B630-312D-43B1-9843-23BA070DD85D}
AppName=Avero
AppVersion={#AppVersion}
AppVerName=Avero {#AppVersion}
AppPublisher=Avero
AppPublisherURL=https://github.com/LnYKLl/Avero
AppSupportURL=https://github.com/LnYKLl/Avero/issues
AppUpdatesURL=https://github.com/LnYKLl/Avero/releases
DefaultDirName={autopf}\Avero
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\..\dist
OutputBaseFilename=Avero-{#AppVersion}-windows-x64-setup
SetupIconFile=..\..\crates\avero-app\assets\logo\avero.ico
UninstallDisplayIcon={app}\avero.exe
UninstallDisplayName=Avero
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
; Closes a running Avero before replacing its files.
CloseApplications=yes
; Code signing, off for now (see the README). The CI turns it on by
; passing /DSign and /Savero=... to iscc when a certificate is set up.
#ifdef Sign
SignTool=avero
SignedUninstaller=yes
#endif

[Languages]
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "avero.exe"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Avero"; Filename: "{app}\avero.exe"; AppUserModelID: "{#AppUserModelID}"
Name: "{autodesktop}\Avero"; Filename: "{app}\avero.exe"; AppUserModelID: "{#AppUserModelID}"; Tasks: desktopicon

[Run]
Filename: "{app}\avero.exe"; Description: "{cm:LaunchProgram,Avero}"; Flags: nowait postinstall skipifsilent
