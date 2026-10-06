//! Local input for the host: keyboard, relative mouse and gamepads, turned
//! into [`LocalInput`] for the main process. The main process decides what
//! goes on the wire (`couch_input::forward`); this side only reports what the
//! user does, and captures the mouse while this member may send it.
//!
//! Local shortcuts (`couch_input::shortcuts`) are never forwarded. Most hold
//! Ctrl+Alt+Shift; F3 alone shows the HUD while keys aren't being sent.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Result, anyhow};
use couch_input::{
    DEV_PAD_IDS, HidUsage, PadButtons, XBOX_360_IDS,
    shortcuts::{self, Shortcut},
};
use couch_proto::{
    ids::Permissions,
    input::{PadSnapshot, mouse_button},
    ipc::{LocalInput, RumbleState, SeatState},
};
use sdl3::{
    GamepadSubsystem,
    event::{Event, WindowEvent},
    gamepad::{Axis, Button, Gamepad},
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
    /// Open pads with Xbox 360 ids: dropped if a host here plugs one in.
    xbox_360: BTreeSet<JoystickId>,
    /// The pad last touched; it drives the seat.
    active: Option<JoystickId>,
    last_pad: PadSnapshot,
    seat: Option<SeatState>,
    focused: bool,
    /// The user freed the mouse with the hotkey.
    mouse_freed: bool,
    captured: bool,
    /// Sub-pixel motion and sub-notch wheel carried to the next event.
    motion_rest: (f32, f32),
    wheel_rest: (f32, f32),
    /// Forwarded and still down, to release when focus goes.
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
    /// Pressed for a hotkey or a capturing click: their releases stay local too.
    swallowed_keys: BTreeSet<u16>,
    swallowed_buttons: BTreeSet<u8>,
}

impl InputCapture {
    /// Call before the SDL joystick subsystem starts: pads keep working while
    /// another window has the focus.
    pub fn allow_background_pads() {
        sdl3::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
    }

    pub fn new(sdl: &sdl3::Sdl) -> Result<Self> {
        let gamepads = sdl.gamepad().map_err(|e| anyhow!("SDL gamepads: {e}"))?;
        Ok(Self {
            gamepads,
            pads: HashMap::new(),
            xbox_360: BTreeSet::new(),
            active: None,
            last_pad: PadSnapshot::default(),
            seat: None,
            focused: false,
            mouse_freed: false,
            captured: false,
            motion_rest: (0.0, 0.0),
            wheel_rest: (0.0, 0.0),
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            swallowed_keys: BTreeSet::new(),
            swallowed_buttons: BTreeSet::new(),
        })
    }

    fn may(&self, permission: Permissions) -> bool {
        self.seat
            .is_some_and(|s| s.holding && s.permissions.contains(permission))
    }

    pub fn set_seat(&mut self, seat: SeatState) {
        self.seat = Some(seat);
        if !seat.holding {
            self.rumble(RumbleState::default());
        }
    }

    pub fn rumble(&mut self, rumble: RumbleState) {
        let id = self.active.or_else(|| self.pads.keys().next().copied());
        if let Some(pad) = id.and_then(|id| self.pads.get_mut(&id)) {
            let scale = |v: u8| u16::from(v) * 257;
            if let Err(e) = pad.set_rumble(scale(rumble.low), scale(rumble.high), RUMBLE_MS) {
                tracing::debug!("rumble failed: {e}");
            }
        }
    }

    /// One line for the HUD.
    pub fn hud_line(&self) -> String {
        let pad = match self.active.and_then(|id| self.pads.get(&id)) {
            Some(pad) => pad.name().unwrap_or_else(|| "gamepad".into()),
            None if self.pads.is_empty() => "no gamepad".into(),
            None => format!("{} gamepad(s)", self.pads.len()),
        };
        match self.seat {
            Some(s) if s.holding => {
                let mut what = Vec::new();
                for (p, name) in [
                    (Permissions::GAMEPAD, "pad"),
                    (Permissions::KEYBOARD, "keyboard"),
                    (Permissions::MOUSE, "mouse"),
                ] {
                    if s.permissions.contains(p) {
                        what.push(name);
                    }
                }
                let mouse = if self.may(Permissions::MOUSE) {
                    if self.captured {
                        format!(
                            "  ({} frees the mouse)",
                            shortcuts::binding(Shortcut::FreeMouse).keys()
                        )
                    } else {
                        "  (click to capture the mouse)".to_string()
                    }
                } else {
                    String::new()
                };
                format!("Controlling: {}  [{pad}]{mouse}", what.join(", "))
            }
            _ => format!("Watching  [{pad}]"),
        }
    }

    /// Handles one event, appending what to send to `out`.
    /// Returns a shortcut for the viewer itself, which goes nowhere else.
    pub fn event(&mut self, event: &Event, out: &mut Vec<LocalInput>) -> Option<Shortcut> {
        match event {
            Event::Window { win_event, .. } => match win_event {
                WindowEvent::FocusGained => self.focused = true,
                WindowEvent::FocusLost => {
                    self.focused = false;
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
                let shortcut = shortcuts::viewer_shortcut(HidUsage(usage), chord)
                    .filter(|_| chord || !self.may(Permissions::KEYBOARD));
                let Some(shortcut) = shortcut else {
                    if self.focused && self.keys.insert(usage) {
                        out.push(LocalInput::Key { usage, down: true });
                    }
                    return None;
                };
                self.swallowed_keys.insert(usage);
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
                if !self.swallowed_keys.remove(&usage) && self.keys.remove(&usage) {
                    out.push(LocalInput::Key { usage, down: false });
                }
            }
            Event::MouseButtonDown { mouse_btn, .. } => {
                let button = button_code(*mouse_btn)?;
                if self.captured {
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
            Event::GamepadAdded { which, .. } => self.open_pad(*which),
            Event::GamepadRemoved { which, .. } => {
                self.pads.remove(which);
                self.xbox_360.remove(which);
                if self.active == Some(*which) {
                    self.active = None;
                    self.send_pad(PadSnapshot::default(), out);
                }
            }
            Event::GamepadButtonDown { which, .. }
            | Event::GamepadButtonUp { which, .. }
            | Event::GamepadAxisMotion { which, .. } => {
                if let Some(pad) = self.pads.get(which) {
                    let snapshot = read_pad(pad);
                    self.active = Some(*which);
                    self.send_pad(snapshot, out);
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

    /// Captures or frees the mouse to match the state. Call after events.
    pub fn sync_capture(&mut self, mouse: &MouseUtil, window: &Window) {
        let want = self.focused && !self.mouse_freed && self.may(Permissions::MOUSE);
        if want != self.captured {
            mouse.set_relative_mouse_mode(window, want);
            self.captured = want;
            self.motion_rest = (0.0, 0.0);
        }
    }

    /// Lets go of everything forwarded (focus lost, seat gone).
    pub fn release_all(&mut self, out: &mut Vec<LocalInput>) {
        for usage in std::mem::take(&mut self.keys) {
            out.push(LocalInput::Key { usage, down: false });
        }
        for button in std::mem::take(&mut self.buttons) {
            out.push(LocalInput::MouseButton {
                button,
                down: false,
            });
        }
    }

    fn open_pad(&mut self, id: JoystickId) {
        let pad = match self.gamepads.open(id) {
            Ok(pad) => pad,
            Err(e) => {
                tracing::warn!("opening a gamepad failed: {e}");
                return;
            }
        };
        let ids = (pad.vendor_id().unwrap_or(0), pad.product_id().unwrap_or(0));
        let name = pad.name().unwrap_or_default();
        let xbox_360 = ids == XBOX_360_IDS;
        if ids == DEV_PAD_IDS || (xbox_360 && host_pads_here()) {
            tracing::info!(
                "skipping {name:?}: a host on this PC has a virtual pad plugged in, and it may be this one"
            );
            return;
        }
        tracing::info!("gamepad {name:?} ({:04x}:{:04x})", ids.0, ids.1);
        if xbox_360 {
            self.xbox_360.insert(id);
        }
        self.pads.insert(id, pad);
    }

    /// Drops Xbox 360 pads once a host on this PC has a virtual pad plugged
    /// in (it went live after this window opened): one of them may be the
    /// host's. Call every second or so.
    pub fn drop_host_pads(&mut self, out: &mut Vec<LocalInput>) {
        if self.xbox_360.is_empty() || !host_pads_here() {
            return;
        }
        for id in std::mem::take(&mut self.xbox_360) {
            tracing::info!(
                "skipping an Xbox 360 pad: a host on this PC has a virtual pad plugged in"
            );
            self.pads.remove(&id);
            if self.active == Some(id) {
                self.active = None;
                self.send_pad(PadSnapshot::default(), out);
            }
        }
    }

    fn send_pad(&mut self, snapshot: PadSnapshot, out: &mut Vec<LocalInput>) {
        if snapshot != self.last_pad {
            self.last_pad = snapshot;
            out.push(LocalInput::Pad(snapshot));
        }
    }
}

/// A host on this PC has a virtual pad plugged in. Host pads report a real
/// Xbox 360 controller's ids (games need them), so while one is plugged in,
/// every pad with those ids is skipped (`couch_platform_win::virtual_pads`).
pub fn host_pads_here() -> bool {
    #[cfg(windows)]
    {
        couch_platform_win::virtual_pads::host_pads_here()
    }
    #[cfg(not(windows))]
    {
        false
    }
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

/// A pad's state in XInput's terms: up is positive on the sticks, triggers
/// run 0 to 255.
fn read_pad(pad: &Gamepad) -> PadSnapshot {
    let mut buttons = PadButtons::empty();
    for (button, bit) in [
        (Button::South, PadButtons::A),
        (Button::East, PadButtons::B),
        (Button::West, PadButtons::X),
        (Button::North, PadButtons::Y),
        (Button::Back, PadButtons::BACK),
        (Button::Guide, PadButtons::GUIDE),
        (Button::Start, PadButtons::START),
        (Button::LeftStick, PadButtons::LEFT_THUMB),
        (Button::RightStick, PadButtons::RIGHT_THUMB),
        (Button::LeftShoulder, PadButtons::LEFT_SHOULDER),
        (Button::RightShoulder, PadButtons::RIGHT_SHOULDER),
        (Button::DPadUp, PadButtons::DPAD_UP),
        (Button::DPadDown, PadButtons::DPAD_DOWN),
        (Button::DPadLeft, PadButtons::DPAD_LEFT),
        (Button::DPadRight, PadButtons::DPAD_RIGHT),
    ] {
        if pad.button(button) {
            buttons |= bit;
        }
    }
    let up = |v: i16| (-i32::from(v)).clamp(-32768, 32767) as i16;
    let trigger = |v: i16| (i32::from(v.max(0)) * 255 / 32767) as u8;
    PadSnapshot {
        buttons: buttons.bits(),
        left_trigger: trigger(pad.axis(Axis::TriggerLeft)),
        right_trigger: trigger(pad.axis(Axis::TriggerRight)),
        left_x: pad.axis(Axis::LeftX),
        left_y: up(pad.axis(Axis::LeftY)),
        right_x: pad.axis(Axis::RightX),
        right_y: up(pad.axis(Axis::RightY)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractions_carry_over() {
        let mut rest = (0.0, 0.0);
        assert_eq!(take_whole(&mut rest, 0.6, -0.4), (0, 0));
        assert_eq!(take_whole(&mut rest, 0.6, -0.7), (1, -1));
        assert!((rest.0 - 0.2).abs() < 1e-5 && (rest.1 + 0.1).abs() < 1e-5);
    }
}
