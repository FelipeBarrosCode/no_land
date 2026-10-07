; Tauri 2.10 supports an explicit installer icon but does not yet expose the
; NSIS uninstaller icon in its configuration schema. MUI reads this definition
; when the uninstaller pages are generated.
!define MUI_UNICON "${__FILEDIR__}\..\icons\icon.ico"

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

  ; The helper checks once per second and allows up to five seconds for the
  ; adapter to close. Waiting here prevents the subsequent Wintun extraction
  ; from failing with "Error opening file for writing".
  Sleep 8000
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro STOP_NOLAND_PROCESSES_BEFORE_FILE_CHANGES
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro STOP_NOLAND_PROCESSES_BEFORE_FILE_CHANGES
!macroend
