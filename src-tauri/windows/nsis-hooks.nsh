!include "WinMessages.nsh"
!define LIOS_PATH_HELPER_SOURCE "${__FILEDIR__}\path-helper.ps1"

!macro LIOS_UPDATE_USER_PATH ACTION
  File /oname=$PLUGINSDIR\lios-path-helper.ps1 "${LIOS_PATH_HELPER_SOURCE}"
  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\lios-path-helper.ps1" "${ACTION}" "$INSTDIR"'
  Pop $0
  Pop $1
  ${If} $0 != 0
    DetailPrint "$1"
    Abort "Unable to ${ACTION} the Lios installation directory in the current-user PATH"
  ${EndIf}
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro LIOS_UPDATE_USER_PATH "add"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro LIOS_UPDATE_USER_PATH "remove"
!macroend
