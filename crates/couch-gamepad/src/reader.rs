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
    local::{
        ControllerChoice, ControllerIds, ControllerKind, LOCAL_PAD_EVERY, LocalController,
        LocalPad, LocalPads,
    },
};
use sdl3::{EventPump, GamepadSubsystem, event::Event, gamepad::Gamepad, joystick::JoystickId};

use crate::{read_pad, rumble_strength};

/// How often pads skipped are looked at again while a follower has none
/// (one plugged anew may have had no player light yet when it came), and a
/// remapper's pad a follower reads is checked against the hosts' serials
/// (M22).
const RECHECK_EVERY: Duration = Duration::from_secs(1);
/// How often a remapper's pad a follower reads is looked up in full again
/// (its device by its player light, then its chain): the Xbox driver's
/// devices are enumerated and opened, so not every second (the second
/// review's finding).
const FULL_CHECK_EVERY: Duration = Duration::from_secs(5);
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
        choice: ControllerChoice,
        on_state: Box<dyn Fn(PadState) + Send>,
        on_device: Box<dyn Fn(Option<String>) + Send>,
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
        choice: ControllerChoice,
        on_state: Box<dyn Fn(PadState) + Send>,
        on_device: Box<dyn Fn(Option<String>) + Send>,
    ) -> Result<Box<dyn LocalPad>, InputError> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let tx = self.sender()?;
        tx.send(Cmd::Follow {
            id,
            choice,
            on_state,
            on_device,
        })
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
    present: Vec<Present>,
    /// Those a follower reads now. Only these are open: opening one can
    /// change it (a DualSense's lights and report mode), so nothing is
    /// opened just to be listed.
    open: Vec<(JoystickId, Gamepad)>,
    /// Pads not read: those whose origin can't be told, and those that may
    /// be our own (a host was plugging one). Looked at again at each
    /// listing, and every second while a follower has none.
    skipped: Vec<JoystickId>,
    /// The hosts' own pads, proven by their serials: never looked at again
    /// (a pad SDL lists keeps its owner, M22).
    ours: Vec<JoystickId>,
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
            ours: Vec::new(),
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
        if self.present.iter().any(|p| p.which == which)
            || self.skipped.contains(&which)
            || self.ours.contains(&which)
        {
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
        let (kind, device) = match whose(path.as_deref(), ids) {
            Whose::Hosts(kind, device) => (kind, device),
            Whose::Ours => {
                self.ours.push(which);
                if first {
                    tracing::info!("not reading {name:?}: it's one of our virtual pads");
                }
                return;
            }
            Whose::Not(why) => {
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
        };
        tracing::info!(
            "controller {name:?} ({:04x}:{:04x}){}",
            ids.vendor,
            ids.product,
            match &device {
                Some(device) => format!(", another program's virtual pad: {device}"),
                None => String::new(),
            }
        );
        self.present.push(Present {
            which,
            ids,
            name,
            path,
            kind,
            device,
        });
    }

    /// Looks again at a remapper's pad a follower reads, or is about to: if
    /// it turns out to be one of ours (M22: something missed in between),
    /// it is set aside for good and closed, and the follower reads it no
    /// more. By its serial against the hosts' markers, cheap; with `full`,
    /// its device looked up anew too (SDL may keep a number across a quick
    /// swap on one XInput slot). Doubt keeps it: dropping it would stop the
    /// host's seat on a passing hitch.
    fn still_theirs(&mut self, which: JoystickId, full: bool) -> bool {
        let Some(at) = self.present.iter().position(|p| p.which == which) else {
            return false;
        };
        let p = &self.present[at];
        let ours = p.device.as_deref().is_some_and(serial_is_a_hosts)
            || (full && matches!(whose(p.path.as_deref(), p.ids), Whose::Ours));
        if !ours {
            return true;
        }
        tracing::warn!(
            "{:?} is one of our virtual pads after all: not read any more",
            p.name
        );
        self.present.remove(at);
        self.open.retain(|o| o.0 != which);
        self.ours.push(which);
        false
    }

    fn removed(&mut self, which: JoystickId) {
        self.present.retain(|p| p.which != which);
        self.open.retain(|p| p.0 != which);
        self.skipped.retain(|s| *s != which);
        self.ours.retain(|s| *s != which);
    }

    /// The remapper pads plugged in now, with their devices.
    fn remapper_pads(&self) -> Vec<Offer> {
        self.present
            .iter()
            .filter(|p| p.kind == ControllerKind::OtherVirtual)
            .map(|p| Offer {
                which: p.which,
                device: p.device.clone(),
            })
            .collect()
    }

    fn list(&self) -> Vec<LocalController> {
        self.present
            .iter()
            .map(|p| LocalController {
                ids: p.ids,
                name: p.name.clone(),
                kind: p.kind,
                device: p.device.clone(),
            })
            .collect()
    }

    fn device(&self, which: JoystickId) -> Option<String> {
        self.present
            .iter()
            .find(|p| p.which == which)
            .and_then(|p| p.device.clone())
    }

    /// A plugged-in controller of `f`'s choice that no other follower reads
    /// (`taken`), as [`choose`] picks it, opened. A remapper's pad is looked
    /// up in full first: one of ours after all is set aside.
    fn attach(&mut self, f: &Follower, taken: &[JoystickId]) -> Option<JoystickId> {
        let offers: Vec<Offer> = self
            .present
            .iter()
            .filter(|p| {
                p.ids == f.choice.ids && p.kind == f.choice.kind && !taken.contains(&p.which)
            })
            .map(|p| Offer {
                which: p.which,
                device: p.device.clone(),
            })
            .collect();
        let remapper = f.choice.kind == ControllerKind::OtherVirtual;
        let which = choose(&offers, remapper.then_some(&f.remapper))?;
        if remapper && !self.still_theirs(which, true) {
            return None;
        }
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

/// A pad on offer to a follower: SDL's number for it, and a remapper's
/// pad's device.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Offer {
    which: JoystickId,
    device: Option<String>,
}

/// What a follower of a remapper's pad remembers (M22), so it never takes
/// another player's pad when its own goes (the reviews' findings).
#[derive(Debug, Default)]
struct RemapperMemory {
    /// Its last pad's device, taken again when it comes back.
    pinned: Option<String>,
    /// The remapper pads that were there when its own went, by number and
    /// by device: another player's, never taken.
    passed_over: Vec<Offer>,
}

impl RemapperMemory {
    fn passes_over(&self, offer: &Offer) -> bool {
        self.passed_over
            .iter()
            .any(|p| p.which == offer.which || (p.device.is_some() && p.device == offer.device))
    }
}

/// Which of `offers` (in the order found) a follower takes. A physical
/// controller's (`remapper` is `None`): the first, two of one model told
/// apart by the order found (M23). A remapper's pad's:
/// - never one passed over (there beside its own when that went);
/// - the one under its last device (the remapper plugging it again);
/// - with no device of its own yet (the session's start), the first;
/// - otherwise another only if it is the one on offer: the remapper
///   plugged its pad again under another id. Two on offer can't be told
///   apart, and none is taken.
///
/// Another program's pad plugged under its last device, the serial its own
/// freed, can't be told from its own coming back: ViGEm gives the lowest
/// free serial.
fn choose(offers: &[Offer], remapper: Option<&RemapperMemory>) -> Option<JoystickId> {
    let Some(memory) = remapper else {
        return offers.first().map(|o| o.which);
    };
    let open: Vec<&Offer> = offers.iter().filter(|o| !memory.passes_over(o)).collect();
    match &memory.pinned {
        None => open.first().map(|o| o.which),
        Some(pinned) => open
            .iter()
            .find(|o| o.device.as_ref() == Some(pinned))
            .or_else(|| (open.len() == 1).then(|| &open[0]))
            .map(|o| o.which),
    }
}

/// Whether a remapper's pad's device carries a serial a host's markers
/// name: one of ours after all.
#[cfg(windows)]
fn serial_is_a_hosts(device: &str) -> bool {
    device
        .rsplit('\\')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .is_some_and(|serial| {
            couch_platform_win::virtual_pads::host_pads()
                .serials
                .contains(&serial)
        })
}

#[cfg(not(windows))]
fn serial_is_a_hosts(_device: &str) -> bool {
    false
}

/// A controller that could play a seat.
struct Present {
    which: JoystickId,
    ids: ControllerIds,
    name: String,
    /// SDL's path for it, to look it up again.
    path: Option<String>,
    kind: ControllerKind,
    /// Another program's virtual pad's device.
    device: Option<String>,
}

/// Whose a gamepad is.
enum Whose {
    /// It can be the host's own: a physical one (P9), or another program's
    /// virtual pad, proven so, with its device (M22).
    Hosts(ControllerKind, Option<String>),
    /// One of the hosts' own virtual pads, by its serial.
    #[cfg_attr(
        not(windows),
        allow(dead_code, reason = "only Windows tells a pad's origin so far")
    )]
    Ours,
    /// Not now, and why: its origin can't be told and it has our pads' ids,
    /// or it may be ours (a host is plugging a pad). Looked at again later.
    Not(&'static str),
}

#[cfg(windows)]
fn whose(path: Option<&str>, ids: ControllerIds) -> Whose {
    use couch_platform_win::{
        pad_origin::{self, Origin},
        virtual_pads,
    };
    let Some((device, chain)) = pad_origin::device_of_gamepad(path, ids) else {
        return if pad_origin::may_be_the_hosts(Origin::Unknown, ids) {
            Whose::Hosts(ControllerKind::Physical, None)
        } else {
            Whose::Not(UNTOLD)
        };
    };
    let hosts = virtual_pads::host_pads();
    if pad_origin::proven_ours(&chain, &hosts) {
        return Whose::Ours;
    }
    if pad_origin::proven_theirs(&chain, &hosts) {
        return Whose::Hosts(ControllerKind::OtherVirtual, Some(device));
    }
    match pad_origin::classify(&chain, &hosts) {
        origin if pad_origin::may_be_the_hosts(origin, ids) => {
            Whose::Hosts(ControllerKind::Physical, None)
        }
        Origin::Ours => Whose::Not("it may be one of our virtual pads"),
        _ => Whose::Not(UNTOLD),
    }
}

#[cfg(not(windows))]
fn whose(_path: Option<&str>, ids: ControllerIds) -> Whose {
    if ids.could_be_ours() {
        Whose::Not(UNTOLD)
    } else {
        Whose::Hosts(ControllerKind::Physical, None)
    }
}

const UNTOLD: &str = "it has our virtual pads' ids, and where it comes from can't be told";

struct Follower {
    id: u64,
    choice: ControllerChoice,
    on_state: Box<dyn Fn(PadState) + Send>,
    on_device: Box<dyn Fn(Option<String>) + Send>,
    /// The pad followed now, if one of the choice is plugged in.
    on: Option<JoystickId>,
    sent: Option<(PadState, Instant)>,
    /// The device last told to `on_device`.
    device: Option<String>,
    /// For a remapper's pad.
    remapper: RemapperMemory,
}

impl Follower {
    /// Its pad went (unplugged, or one of ours after all).
    fn lost(&mut self, remapper_pads: Vec<Offer>) {
        self.on = None;
        if self.choice.kind == ControllerKind::OtherVirtual {
            self.remapper.passed_over = remapper_pads;
        }
    }
}

fn run(mut sdl: Sdl, rx: Receiver<Cmd>) {
    let mut followers: Vec<Follower> = Vec::new();
    let mut rechecked = Instant::now();
    let mut checked_in_full = Instant::now();
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
                    let remapper_pads = sdl.remapper_pads();
                    for f in followers.iter_mut().filter(|f| f.on == Some(which)) {
                        f.lost(remapper_pads.clone());
                    }
                }
                _ => {}
            }
            next = sdl.events.poll_event();
        }
        let now = Instant::now();
        let remapping = followers
            .iter()
            .any(|f| f.on.is_some() && f.choice.kind == ControllerKind::OtherVirtual);
        let lacking = followers.iter().any(|f| f.on.is_none());
        if (remapping || lacking) && now.duration_since(rechecked) >= RECHECK_EVERY {
            rechecked = now;
            if lacking {
                sdl.recheck();
            }
            // The remappers' pads read now, looked at again (M22).
            let full = now.duration_since(checked_in_full) >= FULL_CHECK_EVERY;
            if full {
                checked_in_full = now;
            }
            for f in followers
                .iter_mut()
                .filter(|f| f.choice.kind == ControllerKind::OtherVirtual)
            {
                if let Some(which) = f.on
                    && !sdl.still_theirs(which, full)
                {
                    let remapper_pads = sdl.remapper_pads();
                    f.lost(remapper_pads);
                }
            }
            sdl.close_unread(&followers);
            // These look the pads up on the thread every controller's
            // states wait on.
            let took = now.elapsed();
            if took > Duration::from_millis(5) {
                tracing::debug!("looking at the pads again took {took:?}");
            }
        }
        let mut taken: Vec<JoystickId> = followers.iter().filter_map(|f| f.on).collect();
        for f in &mut followers {
            if f.on.is_none() {
                f.on = sdl.attach(f, &taken);
                if f.on.is_some() {
                    f.remapper.passed_over.clear();
                }
                taken.extend(f.on);
            }
            // Hiding follows another program's pad to each device it has.
            let device = f.on.and_then(|which| sdl.device(which));
            if f.choice.kind == ControllerKind::OtherVirtual && device != f.device {
                if device.is_some() {
                    f.remapper.pinned = device.clone();
                }
                f.device = device.clone();
                (f.on_device)(device);
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
        Cmd::Follow {
            id,
            choice,
            on_state,
            on_device,
        } => followers.push(Follower {
            id,
            choice,
            on_state,
            on_device,
            on: None,
            sent: None,
            device: None,
            remapper: RemapperMemory::default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(n: u32, device: &str) -> Offer {
        Offer {
            which: JoystickId::new(n),
            device: Some(device.into()),
        }
    }

    fn memory(pinned: Option<&str>, passed_over: Vec<Offer>) -> RemapperMemory {
        RemapperMemory {
            pinned: pinned.map(String::from),
            passed_over,
        }
    }

    #[test]
    fn physical_controllers_go_in_the_order_found() {
        let offers = [offer(3, "a"), offer(1, "b")];
        assert_eq!(choose(&offers, None), Some(JoystickId::new(3)));
        assert_eq!(choose(&[], None), None);
    }

    #[test]
    fn a_remapper_follow_takes_its_own_pad_back_and_never_another_players() {
        // The session's start: the first, as the keeper was told.
        let offers = [offer(5, "01"), offer(6, "02")];
        assert_eq!(
            choose(&offers, Some(&memory(None, vec![]))),
            Some(JoystickId::new(5))
        );
        // Its pad (01) went with a friend's (02) beside it; the remapper
        // plugs it again under 01: that one, though the friend's is first.
        let friend = offer(6, "02");
        let back = [friend.clone(), offer(9, "01")];
        let m = memory(Some("01"), vec![friend.clone()]);
        assert_eq!(choose(&back, Some(&m)), Some(JoystickId::new(9)));
        // Back under another id (01 taken meanwhile): the one on offer, the
        // friend's passed over.
        let elsewhere = [friend.clone(), offer(9, "03")];
        assert_eq!(choose(&elsewhere, Some(&m)), Some(JoystickId::new(9)));
        // The friend's pad plugged again (a new number, its device): still
        // passed over, by its device.
        let replugged = [offer(11, "02")];
        assert_eq!(choose(&replugged, Some(&m)), None);
    }

    #[test]
    fn two_unknown_remapper_pads_are_never_guessed_between() {
        // Its pad gone with nothing beside it; two new ones, neither under
        // its device: none taken.
        let m = memory(Some("01"), vec![]);
        let two = [offer(7, "03"), offer(8, "04")];
        assert_eq!(choose(&two, Some(&m)), None);
        // Only the friend's pad there: never taken.
        let friend = offer(6, "02");
        let m = memory(Some("01"), vec![friend.clone()]);
        assert_eq!(choose(&[friend], Some(&m)), None);
    }
}
