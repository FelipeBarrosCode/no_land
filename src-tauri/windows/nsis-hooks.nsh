; Tauri 2.10 supports an explicit installer icon but does not yet expose the
; NSIS uninstaller icon in its configuration schema. MUI reads this definition
; when the uninstaller pages are generated.
!define MUI_UNICON "${__FILEDIR__}\..\icons\icon.ico"
