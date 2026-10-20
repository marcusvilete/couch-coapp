//! The host's controllers, read on one thread that owns SDL for the whole
//! process (the `sdl3` crate allows a single SDL thread). It starts on first
//! use and lives as long as the process. Everything else talks to it over a
//! channel.

use std::{
    sync::{
        Mutex, OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use couch_input::{
    InputError, PadState, Rumble,
    local::{ControllerIds, LOCAL_PAD_EVERY, LocalController, LocalPad, LocalPads},
};
use sdl3::{EventPump, GamepadSubsystem, event::Event, gamepad::Gamepad, joystick::JoystickId};

use crate::{read_pad, rumble_strength};

/// How long the thread waits for an event before looking at its commands
/// and reporting its followers. A pad's events wake it at once; this only
/// delays a command.
const WAIT: Duration = Duration::from_millis(5);
/// The same while nothing is followed: only commands wait on it.
const WAIT_IDLE: Duration = Duration::from_millis(50);
/// Rumble lasts this long unless renewed (the host repeats a running one
/// every 500 ms).
const RUMBLE_MS: u32 = 1_000;

/// The host's controllers. SDL starts on the first call.
pub fn host_pads() -> &'static dyn LocalPads {
    static PADS: OnceLock<SdlPads> = OnceLock::new();
    PADS.get_or_init(|| SdlPads {
        state: Mutex::new(Reader::NotStarted),
    })
}

struct SdlPads {
    state: Mutex<Reader>,
}

enum Reader {
    NotStarted,
    Running(Sender<Cmd>),
    Failed(String),
}

enum Cmd {
    List(Sender<Vec<LocalController>>),
    Follow {
        id: u64,
        ids: ControllerIds,
        on_state: Box<dyn Fn(PadState) + Send>,
    },
    Unfollow(u64),
    Rumble(u64, Rumble),
}

impl SdlPads {
    /// The thread's channel, starting it if need be.
    fn sender(&self) -> Result<Sender<Cmd>, InputError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Reader::NotStarted = *state {
            *state = match start() {
                Ok(tx) => Reader::Running(tx),
                Err(e) => {
                    tracing::warn!("reading this PC's controllers: {e}");
                    Reader::Failed(e)
                }
            };
        }
        match &*state {
            Reader::Running(tx) => Ok(tx.clone()),
            Reader::Failed(e) => Err(InputError::DriverUnavailable(e.clone())),
            Reader::NotStarted => unreachable!("just started"),
        }
    }
}

impl LocalPads for SdlPads {
    fn list(&self) -> Result<Vec<LocalController>, InputError> {
        let (tx, rx) = mpsc::channel();
        let gone = || InputError::DriverUnavailable("the controller reader stopped".into());
        self.sender()?.send(Cmd::List(tx)).map_err(|_| gone())?;
        rx.recv_timeout(Duration::from_secs(2)).map_err(|_| gone())
    }

    fn follow(
        &self,
        ids: ControllerIds,
        on_state: Box<dyn Fn(PadState) + Send>,
    ) -> Result<Box<dyn LocalPad>, InputError> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let tx = self.sender()?;
        tx.send(Cmd::Follow { id, ids, on_state })
            .map_err(|_| InputError::DriverUnavailable("the controller reader stopped".into()))?;
        Ok(Box::new(Followed { id, tx }))
    }
}

struct Followed {
    id: u64,
    tx: Sender<Cmd>,
}

impl LocalPad for Followed {
    fn rumble(&self, rumble: Rumble) {
        let _ = self.tx.send(Cmd::Rumble(self.id, rumble));
    }
}

impl Drop for Followed {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Unfollow(self.id));
    }
}

/// Starts the thread and waits until SDL is up, or says why it isn't.
fn start() -> Result<Sender<Cmd>, String> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::Builder::new()
        .name("host-pads".into())
        .spawn(move || {
            let sdl = match Sdl::start() {
                Ok(sdl) => {
                    let _ = ready_tx.send(Ok(()));
                    sdl
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            run(sdl, rx);
        })
        .map_err(|e| format!("starting its thread: {e}"))?;
    ready_rx
        .recv()
        .map_err(|_| "its thread ended at start".to_string())??;
    Ok(tx)
}

/// SDL with only the gamepads. Fields drop in order: the pads before the
/// subsystems.
struct Sdl {
    /// Controllers plugged in that could play a seat, by what SDL says of
    /// them without opening them.
    present: Vec<(JoystickId, ControllerIds, String)>,
    /// Those a follower reads now. Only these are open: opening one can
    /// change it (a DualSense's lights and report mode), so nothing is
    /// opened just to be listed.
    open: Vec<(JoystickId, Gamepad)>,
    /// Pads not read: virtual ones, and those that could be our own. Looked
    /// at again at each listing.
    skipped: Vec<JoystickId>,
    events: EventPump,
    gamepads: GamepadSubsystem,
    _sdl: sdl3::Sdl,
}

impl Sdl {
    fn start() -> Result<Self, String> {
        // Our window is never SDL's: without this, pads go quiet unless an
        // SDL window has the focus.
        sdl3::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
        let sdl = sdl3::init().map_err(|e| format!("SDL: {e}"))?;
        let gamepads = sdl.gamepad().map_err(|e| format!("SDL gamepads: {e}"))?;
        let events = sdl.event_pump().map_err(|e| format!("SDL events: {e}"))?;
        let mut sdl = Self {
            present: Vec::new(),
            open: Vec::new(),
            skipped: Vec::new(),
            events,
            gamepads,
            _sdl: sdl,
        };
        // Those plugged in already, so the first list has them; their
        // "added" events that follow change nothing.
        for which in sdl.gamepads.gamepads().unwrap_or_default() {
            sdl.added(which);
        }
        Ok(sdl)
    }

    fn added(&mut self, which: JoystickId) {
        self.consider(which, true);
    }

    /// Looks again at the pads skipped: one whose origin couldn't be told
    /// when it came (its player light not set yet, say) may be physical.
    fn recheck(&mut self) {
        for which in std::mem::take(&mut self.skipped) {
            self.consider(which, false);
        }
    }

    fn consider(&mut self, which: JoystickId, first: bool) {
        if self.present.iter().any(|p| p.0 == which) || self.skipped.contains(&which) {
            return;
        }
        // Looked at again but gone meanwhile (its removal not read yet):
        // forgotten, never listed with no ids.
        if !first && self.gamepads.vendor_for_id(which).is_none() {
            return;
        }
        let ids = ControllerIds {
            vendor: self.gamepads.vendor_for_id(which).unwrap_or(0),
            product: self.gamepads.product_for_id(which).unwrap_or(0),
        };
        let name = self
            .gamepads
            .name_for_id(which)
            .unwrap_or_else(|_| "Controller".into());
        let path = self.gamepads.path_for_id(which).ok();
        if let Some(why) = not_the_hosts(path.as_deref(), ids) {
            self.skipped.push(which);
            if first {
                tracing::info!(
                    "not reading {name:?} ({:04x}:{:04x}): {why}",
                    ids.vendor,
                    ids.product
                );
            }
            return;
        }
        tracing::info!(
            "controller {name:?} ({:04x}:{:04x})",
            ids.vendor,
            ids.product
        );
        self.present.push((which, ids, name));
    }

    fn removed(&mut self, which: JoystickId) {
        self.present.retain(|p| p.0 != which);
        self.open.retain(|p| p.0 != which);
        self.skipped.retain(|s| *s != which);
    }

    fn list(&self) -> Vec<LocalController> {
        self.present
            .iter()
            .map(|(_, ids, name)| LocalController {
                ids: *ids,
                name: name.clone(),
            })
            .collect()
    }

    /// The first plugged-in controller with `ids` that no other follower
    /// reads (`taken`), opened: two controllers of one model are told apart
    /// by the order they were found (M23).
    fn attach(&mut self, ids: ControllerIds, taken: &[JoystickId]) -> Option<JoystickId> {
        let which = self
            .present
            .iter()
            .find(|p| p.1 == ids && !taken.contains(&p.0))?
            .0;
        if !self.open.iter().any(|p| p.0 == which) {
            match self.gamepads.open(which) {
                Ok(pad) => self.open.push((which, pad)),
                Err(e) => {
                    tracing::warn!("opening the host's controller: {e}");
                    return None;
                }
            }
        }
        Some(which)
    }

    /// Closes the controllers no follower reads any more.
    fn close_unread(&mut self, followers: &[Follower]) {
        self.open
            .retain(|(which, _)| followers.iter().any(|f| f.on == Some(*which)));
    }

    fn pad(&self, which: JoystickId) -> Option<&Gamepad> {
        self.open.iter().find(|p| p.0 == which).map(|p| &p.1)
    }

    fn pad_mut(&mut self, which: JoystickId) -> Option<&mut Gamepad> {
        self.open
            .iter_mut()
            .find(|p| p.0 == which)
            .map(|p| &mut p.1)
    }
}

/// Why a controller can't be the host's own, if it can't: it's a virtual
/// pad, ours or another program's (P9: only physical controllers play a
/// seat as the host's); or its origin can't be told and it has our pads' ids.
#[cfg(windows)]
fn not_the_hosts(path: Option<&str>, ids: ControllerIds) -> Option<&'static str> {
    use couch_platform_win::pad_origin::{self, Origin};
    let origin = pad_origin::of_gamepad(path, ids);
    if pad_origin::may_be_the_hosts(origin, ids) {
        return None;
    }
    Some(match origin {
        Origin::Ours => "it's one of our virtual pads",
        Origin::OtherVirtual => "it's another program's virtual pad",
        Origin::Physical | Origin::Unknown => UNTOLD,
    })
}

#[cfg(not(windows))]
fn not_the_hosts(_path: Option<&str>, ids: ControllerIds) -> Option<&'static str> {
    ids.could_be_ours().then_some(UNTOLD)
}

const UNTOLD: &str = "it has our virtual pads' ids, and where it comes from can't be told";

struct Follower {
    id: u64,
    ids: ControllerIds,
    on_state: Box<dyn Fn(PadState) + Send>,
    /// The pad followed now, if one with the ids is plugged in.
    on: Option<JoystickId>,
    sent: Option<(PadState, Instant)>,
}

fn run(mut sdl: Sdl, rx: Receiver<Cmd>) {
    let mut followers: Vec<Follower> = Vec::new();
    loop {
        loop {
            match rx.try_recv() {
                Ok(cmd) => command(&mut sdl, &mut followers, cmd),
                Err(TryRecvError::Empty) => break,
                // The process is ending.
                Err(TryRecvError::Disconnected) => return,
            }
        }
        // Following, a pad's events wake us at once; idle, only commands
        // and plugging matter.
        let wait = if followers.is_empty() {
            WAIT_IDLE
        } else {
            WAIT
        };
        let mut next = sdl.events.wait_event_timeout(wait);
        while let Some(event) = next {
            match event {
                Event::GamepadAdded { which, .. } => sdl.added(which),
                Event::GamepadRemoved { which, .. } => {
                    sdl.removed(which);
                    for f in followers.iter_mut().filter(|f| f.on == Some(which)) {
                        f.on = None;
                    }
                }
                _ => {}
            }
            next = sdl.events.poll_event();
        }
        let now = Instant::now();
        let mut taken: Vec<JoystickId> = followers.iter().filter_map(|f| f.on).collect();
        for f in &mut followers {
            if f.on.is_none() {
                f.on = sdl.attach(f.ids, &taken);
                taken.extend(f.on);
            }
            let state =
                f.on.and_then(|which| sdl.pad(which))
                    .map_or(PadState::NEUTRAL, |pad| PadState::from(read_pad(pad)));
            let due = f.sent.is_none_or(|(sent, at)| {
                sent != state || now.duration_since(at) >= LOCAL_PAD_EVERY
            });
            if due {
                (f.on_state)(state);
                f.sent = Some((state, now));
            }
        }
    }
}

fn command(sdl: &mut Sdl, followers: &mut Vec<Follower>, cmd: Cmd) {
    match cmd {
        Cmd::List(reply) => {
            sdl.recheck();
            let _ = reply.send(sdl.list());
        }
        Cmd::Follow { id, ids, on_state } => followers.push(Follower {
            id,
            ids,
            on_state,
            on: None,
            sent: None,
        }),
        Cmd::Unfollow(id) => {
            if let Some(f) = followers.iter().find(|f| f.id == id)
                && let Some(pad) = f.on.and_then(|which| sdl.pad_mut(which))
            {
                let _ = pad.set_rumble(0, 0, 0);
            }
            followers.retain(|f| f.id != id);
            sdl.close_unread(followers);
        }
        Cmd::Rumble(id, rumble) => {
            let on = followers.iter().find(|f| f.id == id).and_then(|f| f.on);
            if let Some(pad) = on.and_then(|which| sdl.pad_mut(which))
                && let Err(e) = pad.set_rumble(
                    rumble_strength(rumble.low_frequency),
                    rumble_strength(rumble.high_frequency),
                    RUMBLE_MS,
                )
            {
                tracing::debug!("rumble failed: {e}");
            }
        }
    }
}
