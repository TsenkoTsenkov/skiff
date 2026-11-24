; Skiff installer for Windows (NSIS 3, Unicode).
;
; Installs for the current user only, so it never asks for administrator
; rights: the program goes to %LOCALAPPDATA%\Programs\Skiff, with a Start
; menu shortcut, an uninstaller and an entry under "Installed apps".
; Settings (%APPDATA%\skiff) are left alone, also on uninstall.
;
; Built by hack/windows-package.ps1:
;   makensis /DVERSION=1.2.3 /DBUILD=42 /DEXE=path\skiff.exe
;            /DLICENSE=path\LICENSE /DICON=path\skiff.ico
;            /DOUTFILE=path\Skiff-windows-x64-setup.exe skiff.nsi
; Silent install: Skiff-windows-x64-setup.exe /S   (what winget runs)

Unicode true
ManifestDPIAware true
RequestExecutionLevel user
SetCompressor /SOLID lzma

!ifndef VERSION
  !error "pass /DVERSION=x.y.z"
!endif
!ifndef BUILD
  !define BUILD 0
!endif
!ifndef EXE
  !error "pass /DEXE=<path of skiff.exe>"
!endif
!ifndef LICENSE
  !error "pass /DLICENSE=<path of the LICENSE file>"
!endif
!ifndef OUTFILE
  !define OUTFILE "Skiff-windows-x64-setup.exe"
!endif

!define APP "Skiff"
!define PUBLISHER "Skiff contributors"
!define URL "https://github.com/TsenkoTsenkov/skiff"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Skiff"

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\Skiff"
; A previous installation's folder wins over the default.
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
ShowInstDetails nevershow
ShowUninstDetails nevershow
BrandingText "${APP} ${VERSION}"

VIProductVersion "${VERSION}.${BUILD}"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "MIT License"
VIAddVersionKey "FileVersion" "${VERSION}.${BUILD}"
VIAddVersionKey "ProductVersion" "${VERSION}"

!include "MUI2.nsh"
!include "FileFunc.nsh"

!ifdef ICON
  !define MUI_ICON "${ICON}"
  !define MUI_UNICON "${ICON}"
!endif
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\skiff.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start Skiff"

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Skiff" SecMain
  SectionIn RO
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  ; (Fails with Abort/Retry/Ignore when Skiff is still running: the file
  ; is locked. Retry after closing it.)
  File "/oname=skiff.exe" "${EXE}"
  File "/oname=LICENSE.txt" "${LICENSE}"
!ifdef NOTICES
  File "/oname=THIRD-PARTY-NOTICES.md" "${NOTICES}"
!endif
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\Skiff.lnk" "$INSTDIR\skiff.exe"

  ; The entry under Settings > Apps > Installed apps.
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "${URL}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\skiff.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" "$0"
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  ; A running Skiff keeps skiff.exe locked: deleting it would fail without a
  ; word, and the rest of this section would then remove the uninstaller
  ; and the "Installed apps" entry, leaving the program behind with nothing
  ; to remove it. So stop before anything is touched. (A running program
  ; cannot be opened for writing; the file is only opened, not changed.)
  IfFileExists "$INSTDIR\skiff.exe" 0 not_running
  ClearErrors
  FileOpen $0 "$INSTDIR\skiff.exe" a
  IfErrors 0 not_locked
    MessageBox MB_OK|MB_ICONEXCLAMATION "Skiff is still running. Close it, then uninstall again." /SD IDOK
    ; (So that a silent uninstall, as winget runs it, reports the failure.)
    SetErrorLevel 1
    Abort "Skiff is still running. Close it, then uninstall again."
  not_locked:
  FileClose $0
  not_running:

  Delete "$SMPROGRAMS\Skiff.lnk"
  Delete "$INSTDIR\skiff.exe"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\THIRD-PARTY-NOTICES.md"
  Delete "$INSTDIR\uninstall.exe"
  ; Only removes the folder when nothing else is in it.
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
SectionEnd
