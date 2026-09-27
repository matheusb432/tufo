; Installs for the current user, without administrator rights, where `just install` does.
; `just windows-installer` defines VERSION, VERSION_NUMERIC, BINARIES_DIR, and OUTFILE.

Unicode true
ManifestDPIAware true
RequestExecutionLevel user
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "FileFunc.nsh"

!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tufo"
; Holds the GUI's *Start at login* entry.
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define RUN_VALUE "Tufo"

Name "Tufo"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\Tufo"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
BrandingText "Tufo ${VERSION}"

VIProductVersion "${VERSION_NUMERIC}"
VIFileVersion "${VERSION_NUMERIC}"
VIAddVersionKey "ProductName" "Tufo"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "Tufo Setup"
; No LegalCopyright version key.
!pragma warning disable 9100

!define MUI_ICON "../../crates/tufo-gui/icons/tufo.ico"
!define MUI_UNICON "../../crates/tufo-gui/icons/tufo.ico"
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TEXT "Setup installs Tufo ${VERSION} for your Windows account.$\r$\n$\r$\nIf Tufo is running, Setup closes it and stops its sounds.$\r$\n$\r$\n$_CLICK"
!define MUI_FINISHPAGE_RUN "$INSTDIR\tufo-gui.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Open Tufo"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; The window goes first so it can't start the server again. The CLI asks the server to fade out
; and leave Discord; taskkill ends one that doesn't answer.
!macro STOP_TUFO CLI
  nsExec::Exec 'taskkill /F /IM tufo-gui.exe'
  Pop $0
  nsExec::Exec '"${CLI}" server kill'
  Pop $0
  nsExec::Exec 'taskkill /F /IM tufo-server.exe'
  Pop $0
!macroend

Section
  ; The new CLI stops whichever server runs, even one this installer didn't place.
  InitPluginsDir
  File "/oname=$PLUGINSDIR\tufo.exe" "${BINARIES_DIR}/tufo.exe"
  !insertmacro STOP_TUFO "$PLUGINSDIR\tufo.exe"

  ; Both clients start the server from their own folder, so all three sit side by side.
  SetOutPath "$INSTDIR"
  File "${BINARIES_DIR}/tufo-gui.exe"
  File "${BINARIES_DIR}/tufo-server.exe"
  CopyFiles /SILENT "$PLUGINSDIR\tufo.exe" "$INSTDIR"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\Tufo.lnk" "$INSTDIR\tufo-gui.exe" "" "" 0 SW_SHOWNORMAL "" \
    "Music and sound effects for tabletop sessions"

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "Tufo"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\tufo-gui.exe,0"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" $0
SectionEnd

; Keeps the library, the settings, and the Discord credentials.
Section "Uninstall"
  !insertmacro STOP_TUFO "$INSTDIR\tufo.exe"

  Delete "$INSTDIR\tufo-gui.exe"
  Delete "$INSTDIR\tufo-server.exe"
  Delete "$INSTDIR\tufo.exe"
  Delete "$INSTDIR\uninstall.exe"
  ; Only an empty folder goes; anything else in it isn't the installer's.
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\Tufo.lnk"
  DeleteRegValue HKCU "${RUN_KEY}" "${RUN_VALUE}"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
SectionEnd
