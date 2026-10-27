# M25a results: trusted devices and the remote console

## Why

**Your aim (2026-10-27):** let a device of yours, usually your laptop, run sessions on your PC at home while you're away, as long as the app runs there. Bonus: as if you were at it, handing seats to friends in the room and to yourself.

The design is [ADR 0010](adr/0010-playing-your-own-pc-from-afar.md). Your choices:
- a device is trusted from a shared room, with a PIN;
- full desktop control for your own devices (M25b);
- trust lasts until revoked.

M25a is the first of three parts: trusted devices, the PIN, and the host's console run from afar. It has no desktop control (M25b), and none of the unattended PC's help (M25c).

A wire change: **revision 10, version 0.21.0**, which doesn't talk to 0.20.0 or earlier.

## What changed

**At the home PC: Settings → Remote play.**
- **"Let my devices run this PC from afar":** a switch, which needs a PIN first. Off locks every device out at once.
- **The PIN:** at least 6 characters, typed twice. Changing it locks devices out until they give the new one.
- **My devices,** with Remove. To add one, you pick a member of one of your rooms, see its code ("1D7E 03BA"), tick that the device shows the same code, and add it.
- **This device:** its own code, and the PCs it accepted (Forget).
- A warning that a trusted device, with the PIN, runs this PC's sessions as you would at it.

**On the laptop, in the room.** A panel per PC that offers remote play:
- **Not accepted yet:** "Ana's PC offers to let this device run it from afar", showing its code to compare, with **It's my PC**. *Found while building:* without this step, any room member could have offered remote play and collected your PIN from the dialog. The laptop now sends a PIN only to a PC it accepted. The ADR records it.
- **Accepted:** **Unlock…**, a PIN dialog that says how long to wait after wrong PINs, and **Forget**.
- **Unlocked:**
  - **Start a session on it…**: the start dialog in remote mode, with the home PC's windows and screens, and no list of its own controllers, since nobody is there;
  - **Lock**;
  - once its session runs, its **host console**, marked "On Ana's PC". Its stream picker, requests, patch bay, menus and stream volume all go to the home PC.
- **The laptop watches the session it started,** at once.

**At the home PC, while a device runs its session:** the console says "Ana's laptop is running this session from afar. The panic key locks it out."

**Underneath:**
- **The wire:** `0x07xx`, between a home PC and a device it trusts only, never relayed:
  - `RemoteOffer`;
  - `RemoteUnlock` / `RemoteUnlockReply`;
  - `RemoteCommand` / `RemoteReply`;
  - `RemoteState`;
  - `RemoteLock`.
- **What an operator may do** is `RemoteOp`, the host's console only:
  - each op becomes the home PC's own window command, through the same checks;
  - `StartSession` goes in the operator's room, without the home PC's controllers;
  - the other ops act only on a session in that room.
- **The home PC's side:**
  - the settings live in `remote-play.json`, never in a backup;
  - the PIN is kept as its Argon2id hash, checked off the actor's thread;
  - wrong PINs: 5 free, then a wait of 1 minute, doubling up to an hour, kept across restarts.
- **The laptop's side:**
  - it keeps its accepted PCs in `remote-hosts.json`;
  - it keeps the PIN in memory for the app's run, and unlocks again by itself after a dropped connection.
- **Locking out:** the panic key at the home PC, a new PIN, removing the device, or turning remote play off locks devices out. All but turning it off make them give the PIN again.

## Checked

- **couch-proto:**
  - the seven messages roundtrip;
  - the fullest state and source list fit a control frame;
  - the PIN never prints;
  - sources are cut to their limits;
  - a state's guest reports are checked as a host checks them.
- **The PIN module (`couch_engine::remote`):**
  - a PIN is normalized and hashed (full-width digits match);
  - wrong PINs wait longer each time, up to an hour;
  - only listed devices are trusted, and only while on with a PIN;
  - device codes;
  - the settings stay out of backups.
- **Session scenarios** (69 pass, six runs in a row after the review's fixes):
  - **`a_trusted_device_runs_the_home_pcs_sessions_from_afar`:**
    - no PIN goes before the laptop accepts its PC;
    - the codes match on both ends;
    - a wrong PIN is refused, then the right one unlocks;
    - a friend gets no offer and can send nothing;
    - the laptop lists the home PC's windows, starts a session (2 pads plugged, nothing hidden) and a stream;
    - both watch;
    - the home console names the laptop, and the laptop's view is the host console;
    - the friend asks, the laptop accepts;
    - the laptop passes a seat to itself and plays the home PC's pad;
    - the laptop ends the session.
  - **`a_device_is_locked_out_by_the_home_pc`:**
    - a dropped connection unlocks again by itself;
    - the panic key, a new PIN, the laptop's own Lock, removing the device and turning remote play off each lock it out;
    - five wrong PINs make it wait, and a right PIN can't skip the wait.
- **The room-security checks:**
  - proto, room and net;
  - rooms (27);
  - sessions three times;
  - relay;
  - `passwords_and_invites_stay_out_of_debug`, extended with the PIN and a remote stream's name.
- **Front end:**
  - vitest (46), with `viaRemote`: the host's commands are wrapped, `startSession` without the home PC's controllers, ours left alone;
  - `npx tsc --noEmit`.
- **The input-safety checks,** with times in their usual ranges:
  - `release-check`: 260 / 93 / 197 / 357 / 0 ms;
  - `input-sink`: everything arrived in order, and nothing blocked got through;
  - `lifecycle-check`: all six passed.
- **`npm run shots`:** the scenarios `remote-offered`, `remote-locked`, `remote-unlocked`, `remote-console` and `remote-home`, and the shots `settings-remote-home`, `settings-remote-new` and `remote-unlock`.

## The review

A fresh review agent, as a security reviewer, found no way for anyone but a trusted, unlocked device on its current connection to make the home PC act. It checked these and found them fine:
- every message's gate: the roster member, the current connection, `trusts`, operators by connection;
- that a device sends a PIN only to a PC it accepted, and that a home PC can only change what the device shows;
- that the ops reach the host's console and nothing else, through `Actor::command` and the seat router;
- the panic key's lock;
- the bounds and size checks;
- that the PIN stays out of logs and backups, and is compared in constant time.

Fixed:
- **A PIN check started before a PIN change could still unlock after it.** A thief with the old PIN, unlocking in a loop, could survive the change. Checks now carry the PIN's generation, and one from before the change opens nothing.
- **Parallel guesses.** Checks were one per connection, so a device in several rooms with the home PC got a guess per room before the wait counted them. Now it is one check per device, whichever connection asks, held until the check ends, and the try is counted before the check runs.
  - **What that fix broke, found by the lock-out scenario failing one run in three:** after a dropped connection, the device's automatic unlock went out on a connection the reconnect then replaced. Its second ask, on the new one, was dropped as "a check is running", and nobody heard the first check's answer. Now every connection of the device that asks while a check runs hears its answer. Six full runs were clean after.
  - The scenario now follows the device's events rather than polling: the moment it is locked can be shorter than a poll.
- **The "session in another room" check missed a session still starting:** it now counts it.
- **The console's state went to operators in other rooms** (guests, controller names, a window's title): it now goes only to operators in the session's room.
- **Two PCs trusting each other** read a panic lock as the other locking itself: only `ByOperator` counts as that now.
- **A late `Unlocked`,** after the device's 30 s wait, left the home PC thinking it unlocked while the device didn't. The device now locks it again. An unlock to a device already unlocked sends the console's state again. Each unlock has its own id, so an old timer can't end a new one.
- **The device normalizes the PIN before sending it,** so its length limit counts the same bytes as the home PC's. The copies of the PIN being set are zeroized.
- **Window titles in `Debug`** (`RemoteOp`, `RemoteWindow`, `RemoteStream`): now `..`, with a test.
- **Source listings,** one at a time per operator.
- **A remote start dialog** closes when its PC locks.
- **The home console's note** says the panic key locks a device out until the PIN is given again, and that turning remote play off keeps it out. A device that knows the PIN can unlock again after the panic key, as the ADR allows. The docs say so.
- **The ADR's message names,** and what is sent when, now match the code.

**A second review, of these fixes,** found:
- no way for another member to be unlocked;
- nothing left stuck (every check ends with its answer, a failed one included);
- the tries counted once per real check.

Fixed:
- A device's self-lock (`ByOperator`) from one that wasn't our operator any more (just locked out by the panic key, say) was read as its PC locking us out. Our view of it then went, while it kept us as an operator. It is now ignored.
- A PIN check running when the panic key was pressed, when the device was removed (and added back), or when remote play was turned off could still unlock afterwards. Each of those now moves the PIN's generation on, as a new PIN does.
- A listing of sources ends only with its own answer (by its command's id), not with any failing command's.
- `ListSources` is refused, like every other op, while the session is in another room: its list carries that session's window's title.

Left: a connection of the device asking while another's check runs hears that check's answer, right or wrong. It's the same authenticated device, so this is only a matter of convenience.

**Left as is:** the remote panels appear above the stage, as the room's callouts do, so the stage moves down when an offer comes or a PC unlocks. `viaRemote` maps every host command today, and passes any other through to this device's engine.

## Your two-PC run (2026-10-27, one network)

- **Worked:**
  - the laptop showed the home PC's console as its host would see it;
  - it started sessions, handed out seats and streamed windows.
- **A screen couldn't be streamed:** "this session has a keyboard-and-mouse seat". That's the rule today (keyboard and mouse only into a window); M25b's desktop control lifts it for your own unlocked device.
- **The streamed game window couldn't be brought to the front from afar.** Keyboard and mouse reach the game only while it is in front, and many games ignore a controller otherwise, so the stream was of little use. Nobody at the home PC means nothing can click it. **New for M25b:** a "Bring to front" action for the streamed window, from the console (here or from afar). Windows lets a background program take the front only in some cases; the app's own injected input may qualify, to check first.
- **Launching games** needs desktop control: M25b.

One network is enough for M25a: remote play rides the room connections friends already use across the internet, and adds nothing of its own to how they connect.

## Still to try

- **Your two-PC run** (the `app-manual-run` skill, Two PCs, "Remote play"): the laptop on a phone's hotspot. At home, set it up and add the laptop. On the laptop, accept, unlock, start a session, stream a window and pass yourself a seat. A friend asks and you accept. Then press the panic key at home.
- **M25b** (desktop control) and **M25c** (start at sign-in, stay awake, the lock report) follow.
