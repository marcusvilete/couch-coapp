; Couch Co-App's steps in Tauri's NSIS installer (M10a; tauri.installer.conf.json,
; bundle > windows > nsis > installerHooks).
;
; After the app installs, each missing driver is offered with a Yes/No
; question saying what won't work without it. A Yes runs the driver's own
; official installer (bundled in drivers\, see drivers/README.md) with
; ExecShellWait "runas": the installer may run as the user ("just me", or a
; standard account), and a plain ExecWait can't start a program that needs
; administrator rights. Already elevated, it simply runs. Nothing is asked
; in a silent, passive or update run. Settings -> Drivers in the app offers
; the same later.
;
; A driver counts as installed when its service is registered.
;
; No note at uninstall that the drivers stay: an upgrade runs the old
; uninstaller without /S, which can't be told from a real uninstall, so the
; note would show in the middle of every upgrade. The docs and Settings ->
; Drivers say it instead.

!macro COUCH_OFFER_DRIVER SERVICE SETUP QUESTION
  ClearErrors
  ReadRegDWORD $0 HKLM "SYSTEM\CurrentControlSet\Services\${SERVICE}" "Start"
  ${IfNot} ${Errors}
    DetailPrint "${SERVICE} is already installed."
  ${ElseIf} ${FileExists} "$INSTDIR\drivers\${SETUP}"
    MessageBox MB_YESNO|MB_ICONQUESTION "${QUESTION}" /SD IDNO IDNO +2
      ExecShellWait "runas" "$INSTDIR\drivers\${SETUP}"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ${IfNot} ${Silent}
  ${AndIf} $PassiveMode <> 1
  ${AndIf} $UpdateMode <> 1
    !insertmacro COUCH_OFFER_DRIVER "ViGEmBus" "vigembus-setup.exe" \
      "Install ViGEmBus (virtual controllers)?$\r$\n$\r$\nWithout it, guests' controllers can't reach games you host. Watching, and a keyboard-and-mouse seat, still work.$\r$\n$\r$\nIts own setup opens and asks for administrator rights. You can also install it later in Settings > Drivers."
    !insertmacro COUCH_OFFER_DRIVER "HidHide" "hidhide-setup.exe" \
      "Install HidHide?$\r$\n$\r$\nIt hides your own controller from games while you stream, so it can play through a seat and never takes a guest's place in co-op games. Without it you play outside the seats.$\r$\n$\r$\nIts own setup opens, asks for administrator rights, and needs Windows restarted. You can also install it later in Settings > Drivers."
  ${EndIf}
!macroend
