//! Local input for the host: keyboard, relative mouse and gamepads, turned
//! into [`LocalInput`] for the main process. The main process decides what
//! goes on the wire (`couch_input::forward`); this side only reports what the
//! user does, and captures the mouse while this member may send it.
//!
//! Local shortcuts (`couch_input::shortcuts`) are never forwarded. Most hold
//! Ctrl+Alt+Shift; F3 alone shows the HUD while keys aren't being sent.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Result, anyhow};
use couch_gamepad::{read_pad, rumble_strength};
use couch_input::{
    DEV_PAD_IDS, HidUsage, XBOX_360_IDS,
    forward::MAX_LOCAL_CONTROLLERS,
    shortcuts::{Shortcut, Shortcuts},
};
use couch_proto::{
    ids::Permissions,
    input::{PadSnapshot, mouse_button},
    ipc::{ControllerNames, HeldSeat, LocalInput, RumbleState, SeatState},
    session::GuestController,
};
use sdl3::{
    GamepadSubsystem,
    event::{Event, WindowEvent},
    gamepad::Gamepad,
    joystick::JoystickId,
    keyboard::Mod,
    mouse::{MouseButton, MouseUtil, MouseWheelDirection},
    video::Window,
};

/// Rumble lasts this long unless renewed. The host repeats a running rumble
/// every 500 ms, so a lost "stop" ends it within a second.
const RUMBLE_MS: u32 = 1_000;

pub struct InputCapture {
    gamepads: GamepadSubsystem,
    pads: HashMap<JoystickId, Gamepad>,
    /// Controllers are opened at all: never in the host's own window (M27d),
    /// which plays nothing.
    with_pads: bool,
    /// Open pads with Xbox 360 ids, our pads' ids, opened while no host here
    /// had pads: looked at every second, closed if one turns out a host's,
    /// settled (dropped from here) once proven not to be.
    xbox_360: BTreeSet<JoystickId>,
    /// Pads not opened because they are, or may be, a host's on this PC:
    /// looked at again every second (P9), and opened once they aren't.
    held_back: BTreeSet<JoystickId>,
    /// Each controller's slot, kept while it is connected (M18): the main
    /// process pins seats to slots (`couch_input::forward`), so a controller
    /// going never moves the others.
    slots: [Option<JoystickId>; MAX_LOCAL_CONTROLLERS],
    /// The pad last touched; with one seat, it drives it.
    active: Option<JoystickId>,
    /// Each pad's state as last sent.
    last_pads: HashMap<JoystickId, PadSnapshot>,
    /// The seats this member holds (M18: possibly several).
    seats: SeatState,
    /// The table in use: the defaults until the main process sends the
    /// user's keys.
    shortcuts: Shortcuts,
    focused: bool,
    /// The mouse stays free until a click into the picture: at the start,
    /// after the focus went elsewhere, and once the hotkey freed it (P18's VM
    /// run: taken as soon as the window had the focus, the bar and its Give
    /// back were out of reach).
    mouse_freed: bool,
    captured: bool,
    /// The system keys are ours (M25b, desktop control).
    keyboard_grabbed: bool,
    /// Drawing on the stream (M27a): the mouse stays free, and nothing of
    /// the keyboard or mouse is forwarded; pads keep playing.
    drawing: bool,
    /// Sub-pixel motion and sub-notch wheel carried to the next event.
    motion_rest: (f32, f32),
    wheel_rest: (f32, f32),
    /// Keys forwarded and still down, and those pressed for a shortcut.
    keys: Keys,
    /// Forwarded and still down, to release when focus goes.
    buttons: BTreeSet<u8>,
    /// Pressed for a capturing click: their releases stay local too.
    swallowed_buttons: BTreeSet<u8>,
}

/// Which keys were forwarded and are still down (to release when focus
/// goes), and which were pressed for a shortcut, whose releases stay here.
#[derive(Debug, Default)]
struct Keys {
    down: BTreeSet<u16>,
    swallowed: BTreeSet<u16>,
}

impl Keys {
    /// A press to forward; `true` if it must go out (it wasn't down).
    /// Never swallowed from then on: its release must go out too.
    fn forward(&mut self, usage: u16) -> bool {
        self.swallowed.remove(&usage);
        self.down.insert(usage)
    }

    /// A press taken by a shortcut: its release stays here.
    fn swallow(&mut self, usage: u16) {
        self.swallowed.insert(usage);
    }

    /// A release; `true` if it must go out (the press went out).
    fn release(&mut self, usage: u16) -> bool {
        !self.swallowed.remove(&usage) && self.down.remove(&usage)
    }

    /// Every key forwarded and down, now let go of.
    fn take_down(&mut self) -> BTreeSet<u16> {
        std::mem::take(&mut self.down)
    }

    /// Releases that never came here (they went to the board, M27a).
    fn forget_swallowed(&mut self) {
        self.swallowed.clear();
    }
}

impl InputCapture {
    /// Call before the SDL joystick subsystem starts: pads keep working while
    /// another window has the focus.
    pub fn allow_background_pads() {
        sdl3::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
    }

    /// Without `with_pads`, no controller is ever opened (the host's own
    /// window, M27d).
    pub fn new(sdl: &sdl3::Sdl, with_pads: bool) -> Result<Self> {
        let gamepads = sdl.gamepad().map_err(|e| anyhow!("SDL gamepads: {e}"))?;
        Ok(Self {
            gamepads,
            pads: HashMap::new(),
            with_pads,
            xbox_360: BTreeSet::new(),
            held_back: BTreeSet::new(),
            slots: [None; MAX_LOCAL_CONTROLLERS],
            active: None,
            last_pads: HashMap::new(),
            seats: SeatState::default(),
            shortcuts: Shortcuts::default(),
            focused: false,
            mouse_freed: true,
            captured: false,
            keyboard_grabbed: false,
            drawing: false,
            motion_rest: (0.0, 0.0),
            wheel_rest: (0.0, 0.0),
            keys: Keys::default(),
            buttons: BTreeSet::new(),
            swallowed_buttons: BTreeSet::new(),
        })
    }

    fn may(&self, permission: Permissions) -> bool {
        self.seats.may(permission)
    }

    pub fn set_seat(&mut self, seats: SeatState) {
        // A controller no longer driving a seat it drove (let go of, or
        // driven by another now) stops shaking.
        let stopped: Vec<u8> = self
            .seats
            .seats
            .iter()
            .filter(|old| !seats.seats.contains(old))
            .filter_map(|old| old.controller)
            .collect();
        for slot in stopped {
            self.shake(slot, 0, 0);
        }
        self.seats = seats;
        // While our keyboard goes to the game, Alt+F4 is the game's (the host
        // holds it back), never a way to close this window by accident. SDL
        // reads the hint at each key press.
        let keyboard = self.may(Permissions::KEYBOARD);
        sdl3::hint::set(
            "SDL_WINDOWS_CLOSE_ON_ALT_F4",
            if keyboard { "0" } else { "1" },
        );
    }

    /// The controller slot driving `seat`, as the main process decided.
    fn slot_for(&self, seat: u8) -> Option<u8> {
        self.seats
            .seats
            .iter()
            .find(|s| s.seat == seat)
            .and_then(|s| s.controller)
    }

    fn shake(&mut self, slot: u8, low: u8, high: u8) {
        let id = self.slots.get(usize::from(slot)).copied().flatten();
        if let Some(pad) = id.and_then(|id| self.pads.get_mut(&id))
            && let Err(e) = pad.set_rumble(rumble_strength(low), rumble_strength(high), RUMBLE_MS)
        {
            tracing::debug!("rumble failed: {e}");
        }
    }

    /// The user's shortcut keys. Keys forwarded and still down stay
    /// tracked, so a key that becomes a shortcut while held still goes up.
    pub fn set_shortcuts(&mut self, shortcuts: Shortcuts) {
        self.shortcuts = shortcuts;
    }

    /// The game shakes `rumble.seat`: the controller driving it shakes.
    pub fn rumble(&mut self, rumble: RumbleState) {
        if let Some(slot) = self.slot_for(rumble.seat) {
            self.shake(slot, rumble.low, rumble.high);
        }
    }

    /// The stats card's Input row: what this viewer drives and with what.
    /// With several pad seats (M18), which controller drives which: "pad A →
    /// seat 1, pad B → seat 3" (seats by number from 1, pads by slot).
    pub fn hud_line(&self) -> String {
        let pad_seats: Vec<&HeldSeat> = self
            .seats
            .seats
            .iter()
            .filter(|s| s.permissions.contains(Permissions::GAMEPAD))
            .collect();
        let pad = if pad_seats.len() > 1 {
            pad_seats
                .iter()
                .map(|s| {
                    let which = s.controller.map_or("no pad".to_string(), |c| {
                        format!("pad {}", char::from(b'A' + c))
                    });
                    format!("{which} → seat {}", s.seat + 1)
                })
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            match self.active.and_then(|id| self.pads.get(&id)) {
                Some(pad) => pad.name().unwrap_or_else(|| "gamepad".into()),
                None if self.pads.is_empty() => "no gamepad".into(),
                None => format!("{} gamepads", self.pads.len()),
            }
        };
        match &self.seats {
            s if s.holding() => {
                let mut what = Vec::new();
                for (p, name) in [
                    (Permissions::GAMEPAD, "pad"),
                    (Permissions::KEYBOARD, "keyboard"),
                    (Permissions::MOUSE, "mouse"),
                ] {
                    if s.may(p) {
                        what.push(name);
                    }
                }
                format!("{} · {pad}", what.join(", "))
            }
            _ => format!("watching · {pad}"),
        }
    }

    /// What the bar tells a player holding the mouse (M21): how to capture
    /// it, or how to free it again. `None` without the mouse.
    pub fn mouse_hint(&self) -> Option<String> {
        if !self.seats.holding() || !self.may(Permissions::MOUSE) || self.drawing {
            return None;
        }
        let what = if self.may(Permissions::DESKTOP) {
            "the keyboard and mouse"
        } else {
            "the mouse"
        };
        Some(if self.captured {
            format!(
                "{} frees {what}",
                self.shortcuts.binding(Shortcut::FreeMouse).keys()
            )
        } else {
            format!("Click the picture to capture {what}")
        })
    }

    /// Handles one event, appending what to send to `out`.
    /// Returns a shortcut for the viewer itself, which goes nowhere else.
    pub fn event(&mut self, event: &Event, out: &mut Vec<LocalInput>) -> Option<Shortcut> {
        match event {
            Event::Window { win_event, .. } => match win_event {
                WindowEvent::FocusGained => self.focused = true,
                WindowEvent::FocusLost => {
                    self.focused = false;
                    // Coming back (Alt+Tab, a click on the title bar) never
                    // captures by itself.
                    self.mouse_freed = true;
                    self.release_all(out);
                }
                _ => {}
            },
            Event::KeyDown {
                scancode: Some(scancode),
                keymod,
                repeat: false,
                ..
            } => {
                let usage = scancode.to_i32() as u16;
                let hotkey = |m: Mod, a, b| m.intersects(a | b);
                let chord = hotkey(*keymod, Mod::LCTRLMOD, Mod::RCTRLMOD)
                    && hotkey(*keymod, Mod::LALTMOD, Mod::RALTMOD)
                    && hotkey(*keymod, Mod::LSHIFTMOD, Mod::RSHIFTMOD);
                // F3 alone is a shortcut only while keys aren't being sent.
                let shortcut = self
                    .shortcuts
                    .viewer_shortcut(HidUsage(usage), chord)
                    .filter(|_| chord || !self.may(Permissions::KEYBOARD));
                let Some(shortcut) = shortcut else {
                    if self.focused && self.keys.forward(usage) {
                        out.push(LocalInput::Key { usage, down: true });
                    }
                    return None;
                };
                self.keys.swallow(usage);
                if shortcut == Shortcut::FreeMouse {
                    self.mouse_freed = !self.mouse_freed;
                    return None;
                }
                return Some(shortcut);
            }
            Event::KeyUp {
                scancode: Some(scancode),
                ..
            } => {
                let usage = scancode.to_i32() as u16;
                if self.keys.release(usage) {
                    out.push(LocalInput::Key { usage, down: false });
                }
            }
            Event::MouseButtonDown { mouse_btn, .. } => {
                let button = button_code(*mouse_btn)?;
                if self.captured {
                    // Forwarded: its release must be too.
                    self.swallowed_buttons.remove(&button);
                    if self.buttons.insert(button) {
                        out.push(LocalInput::MouseButton { button, down: true });
                    }
                } else if self.may(Permissions::MOUSE) {
                    // This click only captures the mouse.
                    self.mouse_freed = false;
                    self.swallowed_buttons.insert(button);
                }
            }
            Event::MouseButtonUp { mouse_btn, .. } => {
                let button = button_code(*mouse_btn)?;
                if !self.swallowed_buttons.remove(&button) && self.buttons.remove(&button) {
                    out.push(LocalInput::MouseButton {
                        button,
                        down: false,
                    });
                }
            }
            Event::MouseMotion { xrel, yrel, .. } if self.captured => {
                let (dx, dy) = take_whole(&mut self.motion_rest, *xrel, *yrel);
                if (dx, dy) != (0, 0) {
                    // Coalesce with the motion just before it.
                    if let Some(LocalInput::MouseMotion { dx: x, dy: y }) = out.last_mut() {
                        *x += dx;
                        *y += dy;
                    } else {
                        out.push(LocalInput::MouseMotion { dx, dy });
                    }
                }
            }
            Event::MouseWheel {
                x, y, direction, ..
            } if self.captured => {
                let sign = if *direction == MouseWheelDirection::Flipped {
                    -1.0
                } else {
                    1.0
                };
                let (dx, dy) = take_whole(&mut self.wheel_rest, x * sign * 120.0, y * sign * 120.0);
                if (dx, dy) != (0, 0) {
                    out.push(LocalInput::Wheel { dx, dy });
                }
            }
            Event::GamepadAdded { which, .. } => {
                if self.with_pads && self.open_pad(*which) {
                    out.push(self.controller_slots());
                }
            }
            Event::GamepadRemoved { which, .. } => {
                if self.close_pad(*which) {
                    out.push(self.controller_slots());
                }
            }
            Event::GamepadButtonDown { which, .. }
            | Event::GamepadButtonUp { which, .. }
            | Event::GamepadAxisMotion { which, .. } => {
                if let Some(pad) = self.pads.get(which) {
                    let snapshot = read_pad(pad);
                    self.active = Some(*which);
                    self.send_pad(*which, snapshot, out);
                }
            }
            _ => {}
        }
        None
    }

    /// The mouse is captured for the game: the pointer is hidden and every
    /// click is the game's.
    pub fn mouse_captured(&self) -> bool {
        self.captured
    }

    /// Captures or frees the mouse to match the state, and with desktop
    /// control (M25b) the keyboard with it: SDL's keyboard grab takes the
    /// Windows key, Alt+Tab and the rest from this PC while the window has
    /// the focus. Only after a click into the picture, never by itself, and
    /// the free shortcut gives both back (Ctrl+Alt+Del always stays this
    /// PC's). Call after events.
    pub fn sync_capture(&mut self, mouse: &MouseUtil, window: &mut Window) {
        let want =
            self.focused && !self.mouse_freed && !self.drawing && self.may(Permissions::MOUSE);
        if want != self.captured {
            mouse.set_relative_mouse_mode(window, want);
            self.captured = want;
            self.motion_rest = (0.0, 0.0);
        }
        let grab = self.captured && self.may(Permissions::DESKTOP);
        if grab != self.keyboard_grabbed {
            window.set_keyboard_grab(grab);
            self.keyboard_grabbed = grab;
            tracing::info!(grab, "the keyboard's system keys");
        }
    }

    /// Drawing mode on or off (M27a). On, whatever keys and buttons were
    /// forwarded go up, and the mouse is freed at the next
    /// [`Self::sync_capture`]; the caller then hands the keyboard and mouse
    /// to the board, never to [`Self::event`].
    pub fn set_drawing(&mut self, on: bool, out: &mut Vec<LocalInput>) {
        if on && !self.drawing {
            self.release_all(out);
        }
        // Releases that came to the board while drawing never reached
        // `event`: a key or button swallowed before (the drawing shortcut's
        // own key, say) would otherwise stay swallowed, and its next release,
        // once forwarded, never be sent: a key held on the host (the M27a
        // review's finding).
        self.keys.forget_swallowed();
        self.swallowed_buttons.clear();
        self.drawing = on;
    }

    /// Whether this member holds the keyboard: drawing is then never theirs
    /// (M27a).
    pub fn has_keyboard(&self) -> bool {
        self.may(Permissions::KEYBOARD)
    }

    /// The window's own chord shortcut a key press is, if it is one, for a
    /// key the board would otherwise take (M27a). The press's release then
    /// stays here too.
    pub fn chord_shortcut(&mut self, event: &Event) -> Option<Shortcut> {
        let Event::KeyDown {
            scancode: Some(scancode),
            keymod,
            repeat: false,
            ..
        } = event
        else {
            return None;
        };
        let held = |a, b| keymod.intersects(a | b);
        let chord = held(Mod::LCTRLMOD, Mod::RCTRLMOD)
            && held(Mod::LALTMOD, Mod::RALTMOD)
            && held(Mod::LSHIFTMOD, Mod::RSHIFTMOD);
        let usage = scancode.to_i32() as u16;
        let shortcut = self.shortcuts.viewer_shortcut(HidUsage(usage), chord);
        // F3 alone too: nothing is forwarded while drawing.
        if shortcut.is_some() {
            self.keys.swallow(usage);
        }
        shortcut.filter(|s| *s != Shortcut::FreeMouse)
    }

    /// Lets go of everything forwarded (focus lost, seat gone).
    pub fn release_all(&mut self, out: &mut Vec<LocalInput>) {
        for usage in self.keys.take_down() {
            out.push(LocalInput::Key { usage, down: false });
        }
        for button in std::mem::take(&mut self.buttons) {
            out.push(LocalInput::MouseButton {
                button,
                down: false,
            });
        }
    }

    /// Opens a pad that connected; `false` if it is skipped.
    fn open_pad(&mut self, id: JoystickId) -> bool {
        let pad = match self.gamepads.open(id) {
            Ok(pad) => pad,
            Err(e) => {
                tracing::warn!("opening a gamepad failed: {e}");
                return false;
            }
        };
        let ids = (pad.vendor_id().unwrap_or(0), pad.product_id().unwrap_or(0));
        let name = pad.name().unwrap_or_default();
        let xbox_360 = ids == XBOX_360_IDS;
        if ids == DEV_PAD_IDS {
            tracing::info!("skipping {name:?}: it has our test pads' ids");
            return false;
        }
        // Only our pads' ids can be a host's pad: the others need no look.
        let check = if xbox_360 {
            host_check(pad.path().as_deref(), ids)
        } else {
            HostCheck::NotAHosts
        };
        if check == HostCheck::MaybeAHosts {
            if self.held_back.insert(id) {
                tracing::info!(
                    "skipping {name:?}: it's a virtual pad of a host on this PC, or may be"
                );
            }
            return false;
        }
        self.held_back.remove(&id);
        tracing::info!("gamepad {name:?} ({:04x}:{:04x})", ids.0, ids.1);
        // The lowest free slot; the others keep theirs.
        if !self.slots.contains(&Some(id)) {
            let Some(slot) = self.slots.iter_mut().find(|s| s.is_none()) else {
                tracing::info!("skipping {name:?}: every controller slot is taken");
                return false;
            };
            *slot = Some(id);
        }
        // Opened with no host here: looked at again once one comes. Proven
        // not a host's: settled.
        if xbox_360 && check == HostCheck::NoHostHere {
            self.xbox_360.insert(id);
        }
        self.pads.insert(id, pad);
        true
    }

    /// Forgets a pad that went; `false` if it wasn't open. Its slot frees;
    /// the others keep theirs.
    fn close_pad(&mut self, id: JoystickId) -> bool {
        self.xbox_360.remove(&id);
        self.held_back.remove(&id);
        self.last_pads.remove(&id);
        for slot in &mut self.slots {
            if *slot == Some(id) {
                *slot = None;
            }
        }
        if self.active == Some(id) {
            self.active = None;
        }
        self.pads.remove(&id).is_some()
    }

    /// The slots with a controller, for the main process (after each change).
    fn controller_slots(&self) -> LocalInput {
        let mask = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_some())
            .fold(0u8, |mask, (i, _)| mask | 1 << i);
        LocalInput::ControllerSlots(mask)
    }

    /// The connected controllers for the bar's choice (P13): each with the
    /// seat it drives, as the main process decided.
    pub fn hud_controllers(&self) -> Vec<crate::hud::HudController> {
        self.controller_names()
            .controllers
            .into_iter()
            .map(|c| crate::hud::HudController {
                seat: self
                    .seats
                    .seats
                    .iter()
                    .find(|s| s.controller == Some(c.slot))
                    .map(|s| s.seat),
                slot: c.slot,
                name: c.name,
            })
            .collect()
    }

    /// The connected controllers' names, by slot (P12): sent after
    /// [`Self::controller_slots`], for the host's patch bay.
    pub fn controller_names(&self) -> ControllerNames {
        ControllerNames {
            controllers: self
                .slots
                .iter()
                .enumerate()
                .filter_map(|(slot, id)| {
                    let pad = self.pads.get(&(*id)?)?;
                    Some(GuestController {
                        slot: slot as u8,
                        name: couch_proto::session::controller_name(
                            &pad.name().unwrap_or_default(),
                        ),
                    })
                })
                .collect(),
        }
    }

    /// Looks again at the Xbox 360 pads, our pads' ids (P9): one open that
    /// turns out a host's on this PC (a host went live after this window
    /// opened, say) closes; one held back that turns out physical or another
    /// program's (its player light set since, or the host gone) opens. Call
    /// every second or so.
    pub fn recheck_host_pads(&mut self, out: &mut Vec<LocalInput>) {
        let mut changed = false;
        for id in self.xbox_360.clone() {
            let path = self.pads.get(&id).and_then(|pad| pad.path());
            match host_check(path.as_deref(), XBOX_360_IDS) {
                HostCheck::NoHostHere => {}
                HostCheck::NotAHosts => {
                    self.xbox_360.remove(&id);
                }
                HostCheck::MaybeAHosts => {
                    tracing::info!(
                        "skipping an Xbox 360 pad: it's a virtual pad of a host on this PC, or may be"
                    );
                    changed |= self.close_pad(id);
                    self.held_back.insert(id);
                }
            }
        }
        for id in self.held_back.clone() {
            let path = self.gamepads.path_for_id(id).ok();
            if host_check(path.as_deref(), XBOX_360_IDS) != HostCheck::MaybeAHosts
                && self.open_pad(id)
            {
                tracing::info!("reading an Xbox 360 pad after all: it isn't a host's");
                changed = true;
            }
        }
        if changed {
            out.push(self.controller_slots());
        }
    }

    /// One pad's state, by its slot, if it changed.
    fn send_pad(&mut self, id: JoystickId, snapshot: PadSnapshot, out: &mut Vec<LocalInput>) {
        let Some(slot) = self.slots.iter().position(|s| *s == Some(id)) else {
            return;
        };
        if self.last_pads.insert(id, snapshot) != Some(snapshot) {
            out.push(LocalInput::Pad {
                controller: slot as u8,
                pad: snapshot,
            });
        }
    }
}

/// Whether the pad SDL lists with `path` and `ids` is, or may be, a virtual
/// pad of a host on this PC: reading it would send the host's own pad back
/// to it. Host pads report a real Xbox 360 controller's ids (games need
/// them), so the ids can't tell; where the pad comes from can (P9,
/// `couch_platform_win::pad_origin`): a physical controller or another
/// program's virtual pad is read. Where that can't be told, any Xbox 360 pad
/// counts as a host's while a host here has pads, as before P9.
pub fn may_be_hosts(path: Option<&str>, ids: (u16, u16)) -> bool {
    host_check(path, ids) == HostCheck::MaybeAHosts
}

/// What [`may_be_hosts`] found, and whether it proved anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostCheck {
    /// No host here has pads: nothing to tell yet.
    NoHostHere,
    /// Physical or another program's, with a host here: that won't change.
    NotAHosts,
    MaybeAHosts,
}

#[cfg(windows)]
fn host_check(path: Option<&str>, ids: (u16, u16)) -> HostCheck {
    use couch_input::local::ControllerIds;
    use couch_platform_win::{
        pad_origin::{self, Origin},
        virtual_pads,
    };
    let hosts = virtual_pads::host_pads();
    if !hosts.any() {
        return HostCheck::NoHostHere;
    }
    let controller = ControllerIds {
        vendor: ids.0,
        product: ids.1,
    };
    match pad_origin::of_gamepad_given(path, controller, &hosts) {
        Origin::Ours => HostCheck::MaybeAHosts,
        Origin::Physical | Origin::OtherVirtual => HostCheck::NotAHosts,
        Origin::Unknown if ids == XBOX_360_IDS => HostCheck::MaybeAHosts,
        Origin::Unknown => HostCheck::NotAHosts,
    }
}

/// Linux (M16c): a host's pads name themselves (`phys` `couch-coapp/…`,
/// `couch_platform_linux::pads`), so a pad is proven ours or not by its
/// event node; one that can't be read is treated as before a host existed.
#[cfg(target_os = "linux")]
fn host_check(path: Option<&str>, _ids: (u16, u16)) -> HostCheck {
    match path.and_then(couch_platform_linux::pads::is_ours) {
        Some(true) => HostCheck::MaybeAHosts,
        Some(false) => HostCheck::NotAHosts,
        None => HostCheck::NoHostHere,
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn host_check(_path: Option<&str>, _ids: (u16, u16)) -> HostCheck {
    HostCheck::NoHostHere
}

pub fn button_code(button: MouseButton) -> Option<u8> {
    Some(match button {
        MouseButton::Left => mouse_button::LEFT,
        MouseButton::Right => mouse_button::RIGHT,
        MouseButton::Middle => mouse_button::MIDDLE,
        MouseButton::X1 => mouse_button::BACK,
        MouseButton::X2 => mouse_button::FORWARD,
        _ => return None,
    })
}

/// Adds `(x, y)` to `rest` and takes out the whole units.
fn take_whole(rest: &mut (f32, f32), x: f32, y: f32) -> (i32, i32) {
    rest.0 += x;
    rest.1 += y;
    let whole = (rest.0.trunc(), rest.1.trunc());
    rest.0 -= whole.0;
    rest.1 -= whole.1;
    (whole.0 as i32, whole.1 as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The M27a review's finding: a shortcut's key whose release went to the
    /// board (drawing mode) must not stay swallowed, or a later press of it,
    /// forwarded, would never be let go of on the host.
    #[test]
    fn a_key_forwarded_after_its_shortcut_is_always_released() {
        const D: u16 = 0x07;
        let mut keys = Keys::default();
        // Ctrl+Alt+Shift+D turns drawing on: swallowed; its release goes to
        // the board, never here.
        keys.swallow(D);
        // Drawing ends; later, holding the keyboard, D alone is typed.
        keys.forget_swallowed();
        assert!(keys.forward(D));
        assert!(keys.release(D), "the release goes out");
        // Even without the drawing switch forgetting: a forwarded press is
        // never swallowed.
        keys.swallow(D);
        assert!(keys.forward(D));
        assert!(keys.release(D));
        assert!(keys.take_down().is_empty());
        // A shortcut's own press and release stay here.
        keys.swallow(D);
        assert!(!keys.release(D));
    }

    #[test]
    fn fractions_carry_over() {
        let mut rest = (0.0, 0.0);
        assert_eq!(take_whole(&mut rest, 0.6, -0.4), (0, 0));
        assert_eq!(take_whole(&mut rest, 0.6, -0.7), (1, -1));
        assert!((rest.0 - 0.2).abs() < 1e-5 && (rest.1 + 0.1).abs() < 1e-5);
    }
}
