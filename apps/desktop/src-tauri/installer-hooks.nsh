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
;
; Uninstalling with "Delete the application data" ticked (P4): Tauri's box
; deletes only its own folders, named after the bundle id. Ours, with every
; profile (identity, rooms, settings, logs), is %LOCALAPPDATA%\CouchCoApp of
; the user running the uninstaller. It goes too, after a Yes/No question.
; If a session that ended abruptly left a controller hidden or keys held
; (crash markers, `pid-*`, which the app undoes at its next start and nothing
; else would), the question says to keep the data and start the app once
; first, and No is its default: the app can't always clear a marker (HidHide
; uninstalled since), so deleting anyway stays possible. Never during an
; update; the box can't be ticked in a silent or passive run.

; Sets RESULT to 1 if folder DIR holds a crash marker (`pid-*`), else 0.
!macro COUCH_HAS_MARKERS DIR RESULT
  StrCpy ${RESULT} 0
  ClearErrors
  FindFirst $R0 $R1 "${DIR}\pid-*"
  ${If} $R1 != ""
    StrCpy ${RESULT} 1
  ${EndIf}
  ${If} $R0 != ""
    FindClose $R0
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Start with Windows (M25c) goes with the app, or Windows tries the missing
  ; program at every sign-in: the current user's Run value, which the app
  ; writes in either install mode. Never during an update, which would turn
  ; the user's switch off.
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Couch Co-App"
  ${EndIf}
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    SetShellVarContext current
    ${If} ${FileExists} "$LOCALAPPDATA\CouchCoApp\*.*"
      !insertmacro COUCH_HAS_MARKERS "$LOCALAPPDATA\CouchCoApp\hidden-controllers" $R2
      !insertmacro COUCH_HAS_MARKERS "$LOCALAPPDATA\CouchCoApp\held-input" $R3
      ${If} $R2 = 1
      ${OrIf} $R3 = 1
        MessageBox MB_YESNO|MB_ICONEXCLAMATION|MB_DEFBUTTON2 "Delete Couch Co-App's data anyway?$\r$\n$\r$\nA session that ended abruptly left your controller hidden from games, or keys held down, and the app puts that right when it starts. Best: answer No, install Couch Co-App again, start it once and close it, then uninstall with $\"Delete the application data$\" ticked.$\r$\n$\r$\nYes deletes every profile of this Windows user (identity, rooms, settings, logs); a controller left hidden then needs HidHide's own app to show it again.$\r$\n$\r$\nIt is in $LOCALAPPDATA\CouchCoApp." /SD IDNO IDNO +2
          RmDir /r "$LOCALAPPDATA\CouchCoApp"
      ${Else}
        MessageBox MB_YESNO|MB_ICONQUESTION "Delete Couch Co-App's data too?$\r$\n$\r$\nEvery profile of this Windows user goes: its identity, its rooms, settings and logs. A new install starts as someone new.$\r$\n$\r$\nRooms you own stay as they are for their members, but no one can join them any more. The rooms you're in still list you, until their owner bans you.$\r$\n$\r$\nIt is in $LOCALAPPDATA\CouchCoApp." /SD IDNO IDNO +2
          RmDir /r "$LOCALAPPDATA\CouchCoApp"
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend

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
