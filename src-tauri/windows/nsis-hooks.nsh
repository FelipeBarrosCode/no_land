; Tauri 2.10 supports an explicit installer icon but does not yet expose the
; NSIS uninstaller icon in its configuration schema. MUI reads this definition
; when the uninstaller pages are generated.
!define MUI_UNICON "${__FILEDIR__}\..\icons\icon.ico"
!include "LogicLib.nsh"

; The elevated network helper keeps wintun.dll loaded after the desktop app
; exits. Ask it to shut down cooperatively before NSIS replaces or removes
; bundled files. The runtime directory is under the current user's roaming
; data, so this does not require elevation and does not touch state.json.
!macro STOP_NOLAND_PROCESSES_BEFORE_FILE_CHANGES
  DetailPrint "Stopping Noland Connect background processes..."
  nsExec::ExecToLog 'taskkill.exe /F /T /IM "noland-connect.exe"'

  CreateDirectory "$APPDATA\com.noland.connect\wireguard\gotatun-runtime"
  ClearErrors
  FileOpen $0 "$APPDATA\com.noland.connect\wireguard\gotatun-runtime\stop.request" w
  IfErrors +3
  FileWrite $0 "stop$\r$\n"
  FileClose $0

  ; Do not assume that eight seconds released wintun.dll. Address/route
  ; cleanup can take longer, or the helper may be stuck. Fail safely rather
  ; than replacing a binary/library which an elevated helper still owns.
  nsExec::ExecToStack /TIMEOUT=25000 '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "$$deadline = [DateTime]::UtcNow.AddSeconds(20); while (Get-Process -Name noland-net-helper* -ErrorAction SilentlyContinue) { if ([DateTime]::UtcNow -ge $$deadline) { exit 1 }; Start-Sleep -Milliseconds 250 }; exit 0"'
  Pop $0
  Pop $1
  ${If} $0 != "0"
    DetailPrint "Network helper shutdown could not be confirmed: $1"
    MessageBox MB_OK|MB_ICONSTOP "The Noland network helper is still running or its shutdown could not be verified. Disconnect the tunnel and close any other Noland sessions, then retry. No bundled files have been replaced or removed."
    Abort
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro STOP_NOLAND_PROCESSES_BEFORE_FILE_CHANGES
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro STOP_NOLAND_PROCESSES_BEFORE_FILE_CHANGES
!macroend
