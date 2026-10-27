# ADR 0010: Playing your own PC from afar

**Status:** accepted (2026-10-27), for M25; M25a built ([results](../m25a-results.md)). The author's choices:
- a device is trusted by picking it from a shared room, with a PIN;
- your own devices may control the whole desktop;
- trust lasts until revoked.

## Context

The friends' run of 0.17.1 asked to play a gaming PC at home from a laptop while travelling, with nobody at home. The author's aims:
- your other device can run sessions on the home PC, as long as its app is running;
- ideally as if you were there: handing seats to friends in the room, and to yourself.

Today none of that works:
- **Only someone at the host's PC can start a session,** choose what to stream, answer requests or pass seats. Each is a command from the app's own window (`Command::StartSession`, `StartStream`, `AnswerControl`, `PassControl`, …).
- **Guests reach only the game.** Keyboard and mouse reach only the streamed game window, while it is in front. The Windows key, Alt+Tab, Alt+F4 and their kind never go through. So from afar nobody could start a game, or close a dialog in front of it.
- **The app never tries to keep a PC reachable.** Nothing starts it at sign-in, keeps the PC awake, or says when Windows has locked it.

## Decision

### Trusted devices

- **"Your devices" are other identities of yours, marked on the home PC (H).** At H, Settings → Remote play → Add a device:
  - you pick a member of a room H shares with that device (the online ones listed first);
  - both screens show a short code derived from that member's id, and you compare them, so a member with the same name can't pass for it;
  - you set a **PIN** (at least 6 characters).

  Only someone at H can add a device, remove one, change the PIN, or switch remote play off. None of these exist as remote operations.
- **The device accepts H too, once.** H's offer only asks, on the device, whether that member is your own PC, showing its code to compare with H's Settings. The device sends a PIN only to a PC it accepted (`remote-hosts.json`), so a room member pretending to be your PC never gets one. (Found while building M25a.)
- **Trust lasts until revoked at H.** Nothing expires. The PIN is what stands between a lost laptop and your PC.
- **The PIN:**
  - H stores only its Argon2id hash, with the trusted ids, in `remote-play.json`;
  - none of this goes in a profile's backup: a restored PC trusts nobody until you add the devices again;
  - the remote device asks for the PIN once per app start and keeps it in memory only;
  - it sends the PIN over its room connection to H. That connection is iroh's end-to-end encrypted QUIC, authenticated to H's key, so only H can read it.
- **Guessing is slowed:** after 5 wrong PINs, H waits 1 minute before the next try, doubling up to an hour. The count is kept across restarts. A right PIN clears it.
- **An unlock is per connection.** A trusted device that gave the right PIN is H's **operator** on that connection until it closes, the device leaves the room, or someone at H presses the panic key.

### The remote console

- **The operator sends H the host's commands; H's engine runs them as if from its own window.**
  - **What the operator may do:**
    - list sources;
    - start and end a session;
    - start and stop streams, and set the stream's sound level;
    - answer requests, pass seats, take them back, Play here;
    - allow control again, and stop waiting for pads;
    - start or stop desktop control (below).
  - **What it may never do:** anything else. No rooms (invites, bans, handovers), settings, shortcuts, network, drivers, backups or remote play itself.
- **H stays the host.** It is the only writer of its session's facts, as before. The operator's commands are requests to H's engine, which checks them exactly as it checks its own window's. Someone at H can use H's own console at the same time; the engine takes the commands in the order they arrive.
- **The operator sees H's host console:** H sends it what its console shows that members don't otherwise hear (`RemoteState`: requests, guests' controllers, the stream, the sound level, pads) on every change, only to operators in the session's room, and its windows and screens when asked. So, from the laptop:
  - you start a session in a room you share with H, and stream a window or a screen;
  - friends in that room watch it as they watch any of H's sessions, and ask for seats;
  - you answer them and pass seats to them, or to your laptop, which watches the session like any viewer.
- **On the wire:** control messages of their own, between H and the operator only, never relayed:
  - `RemoteOffer`, `RemoteUnlock` / `RemoteUnlockReply`, `RemoteLock`;
  - `RemoteCommand` / `RemoteReply`;
  - `RemoteState`.

  H acts on them only from a trusted id, on the connection that was unlocked, while that id is on the room's roster. A new wire revision.

### Desktop control (your devices only)

- **The operator may turn on desktop control:** a screen is streamed, and the operator's keyboard and mouse reach the whole desktop.
- **It changes two of the project's safety rules, for the operator alone:**
  - keyboard and mouse no longer need the streamed window in front;
  - the Windows key, Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc and Alt+F4 go through. A desktop can't be used without them.

  The gamepad Guide button stays blocked: Windows' Game Bar takes it.
- **How:**
  - a permission bit, `Permissions::DESKTOP`, which the seat router reads to skip those two checks;
  - H's engine adds it only to a grant of the keyboard-and-mouse seat to the unlocked operator, and only after the operator asked for desktop control;
  - `HostSession` refuses it for anyone else;
  - its router tests and the session property test keep that so.
- **Getting your keys back on the laptop.** To send the Windows key, Alt+Tab and the rest, the laptop's stream window must take them before the laptop's own Windows does. So there must always be a way out:
  - **Keyboard and mouse are taken only once you click into the picture,** as the mouse is today, and only while the stream window is in front. Nothing is taken by itself: not when desktop control starts, and not when the window opens.
  - **The "Free the mouse" shortcut becomes "Free the keyboard and mouse"** (Ctrl+Alt+Shift+M by default, movable in Settings like any shortcut). It gives every key back to the laptop at once: the Windows key, Alt+Tab and the rest act on the laptop again. A click into the picture takes them again. Shortcuts are never sent to the remote PC, so it can't be swallowed.
  - **The stream window losing the front gives the keys back too:** minimizing it, closing it, or the laptop locking. So does the connection dropping.
  - **Ctrl+Alt+Del always stays on the laptop:** Windows lets no program take it. It is the way out of last resort.
  - **The bar says which key frees them** while they are taken, as it does for the mouse today. Its row comes up whatever the bar setting is.
  - **How:** SDL3's keyboard grab (`SDL_SetWindowKeyboardGrab`), which on Windows takes the system keys with a low-level hook while the window has the focus. M25b checks that it works in our stream window, which reads raw input. The `windows-ffi-pitfalls` skill notes that raw input blinds a process's own low-level hooks. If the grab can't see the keys, the stream window gets a hook of its own, kept apart from raw input.
- **Unchanged for everyone else:** guests' rules don't move. One holder at a time has keyboard and mouse. Every handoff, disconnect and teardown releases held keys. Every key injected is still recorded (`held_input`).
- **Visible at home:**
  - while the operator controls the desktop, H shows a banner, "Marcus's laptop is controlling this PC · the panic key ends it": unfocusable, click-through, and kept out of the capture (the host overlay's rules);
  - the panic key, pressed by anyone at H, ends desktop control and the operator's unlock: the operator must give the PIN again;
  - injected keys never press the panic key (as before), so the operator can't defeat it.

### Unattended PC

- **Start at sign-in:** an option, on with remote play. The app starts to the notification area.
- **Close to the notification area:** while remote play is on, closing the window hides it instead of quitting. The tray icon's menu quits.
- **Stay awake** while remote play is on (`SetThreadExecutionState`, the system), and keep the display on during a remote session: a sleeping display stops the capture.
- **Say what can't be done:** H tells the operator when Windows is locked, so the operator sees "Your PC is locked: Windows won't let anything in until someone unlocks it at home", not a black stream.

### What can't work

- **Windows keeps some things out of every program's reach:** the lock screen, sign-in, UAC prompts and Ctrl+Alt+Del. They are on the secure desktop, which can't be captured or driven. H must stay signed in and unlocked.
- **A PC asleep, off, or without the app running** can't be reached. Waking it from afar (Wake-on-LAN) needs another device at home; this ADR leaves it out.

## Threat model

| Who | What they can do | What stops them |
|---|---|---|
| A room member, a friend | Nothing more than today | Remote messages count only from a trusted id; guests can never get `DESKTOP` |
| A stranger | Nothing: they can't reach a room | Admission, as before |
| A thief with the laptop, without the PIN | Guess, slowly | 5 tries, then waits doubling up to an hour; revoke at H once home, or ask someone there |
| A room member pretending to be your PC | Offer remote play to your laptop | The laptop asks whether it's your PC, showing its code; it sends a PIN only to a PC you accepted |
| A thief with the laptop and the PIN | **Anything you could do at your PC** while it is unlocked: open files, browsers with saved sign-ins, buy in Steam | Someone at home sees the banner and can press the panic key; a locked PC lets nothing in. Choose a PIN nobody sees you type |
| Malware on the laptop | The same as a thief with the PIN, once you type it | Out of our reach: the same as for any remote-desktop tool |
| Someone at home | End it with the panic key; use H's own console | As intended |

A trusted device is as powerful as a remote-desktop tool. Settings → Remote play says so before you add one.

## Consequences

- **Built in three parts,** each its own PR, with review agents and the room-security and input-safety checks:
  - **M25a:** trusted devices, the PIN and the remote console (no desktop control);
  - **M25b:** desktop control, its permission bit and the banner;
  - **M25c:** the unattended PC: start at sign-in, the notification area, staying awake, the lock report.
- **A wire revision** (M25a), and a minor version.
- **CLAUDE.md's safety invariants gain the operator's exception,** stated exactly as above. Everything else in them holds.
- **Testing needs two PCs,** both of the author's: one at home, one on another network (a phone's hotspot) for "from afar".
