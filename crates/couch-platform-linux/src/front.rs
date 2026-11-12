//! Which window is in front (M16c-2, ADR 0012): what a Linux host needs to
//! keep a guest's keys on the game, which Wayland tells no program.
//!
//! The desktop's own shell tells us, through a helper that only reports:
//! on KDE a KWin script, written for the session and loaded over D-Bus
//! (`org.kde.kwin.Scripting`), unloaded when it ends (and at the next start
//! by its name, if a crash left it); on GNOME our extension
//! (`front@couchcoapp.com`, which Settings → Drivers installs), asked to
//! [`watch`]. Each pushes, by method calls to this process's connection, the
//! windows (id, title, program, size) and which is active with whether the
//! pointer is over it. A push counts only from the current owner of the
//! shell's bus name (`org.kde.KWin`, `org.gnome.Shell`), followed from the
//! bus's own `NameOwnerChanged`, and only with the session's token.
//!
//! **A helper that stops speaking counts for nothing.** Each pushes what is
//! in front at least every second, and what it said counts for [`STALE`]
//! after its last push; its shell leaving the bus ends it at once, and our
//! extension says nothing is in front as GNOME turns it off (at the lock
//! screen). A quiet helper is started again (GNOME's watch after the lock
//! screen, KWin's script after KWin restarts or another program unloads
//! it), the named game keeping its id.
//!
//! Nothing here acts on a window. [`focus`] and [`exists`] read the last
//! push, never the bus, so they cost nothing per input. Everything on the
//! bus runs on the portals' runtime (`portal::runtime`).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use ashpd::{
    zbus::{self, MatchRule, fdo::DBusProxy, message::Type as MessageType, names::BusName},
    zvariant::OwnedValue,
};
use couch_input::{Bounds, confine::PointerReport};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::{Notify, oneshot};

/// Our object and interface, on our connection (pushes) and the GNOME
/// extension's (`Watch`).
const PATH: &str = "/com/couchcoapp/Front";
const IFACE: &str = "com.couchcoapp.Front";
const KWIN: &str = "org.kde.KWin";
const SHELL: &str = "org.gnome.Shell";
/// The GNOME extension's id.
pub const EXTENSION_UUID: &str = "front@couchcoapp.com";

/// The KWin script, with `@SERVICE@` and `@TOKEN@` filled in per session.
const KWIN_SCRIPT: &str = include_str!("../front/kwin.js");
/// The GNOME extension's files.
const EXTENSION_METADATA: &str = include_str!("../front/gnome/metadata.json");
const EXTENSION_JS: &str = include_str!("../front/gnome/extension.js");
/// The version in its `metadata.json`: GNOME runs the version it read at
/// log-in, so a newer one installed since counts only after logging in again
/// (an older one says nothing of the pointer, which keeps a guest's in the
/// game window: P18).
const EXTENSION_VERSION: f64 = 5.0;

/// The largest push taken: a list of a few hundred windows.
const MAX_PUSH: usize = 256 * 1024;
/// Windows kept from one push, and the longest title or program name.
const MAX_WINDOWS: usize = 512;
const MAX_TEXT: usize = 512;
/// The longest helper id for a window taken (KWin's are UUIDs, GNOME's
/// numbers); a window with a longer one is left out, and an active one
/// counts as a window never listed.
const MAX_HELPER_ID: usize = 64;
/// Helper ids remembered before those of windows gone are forgotten.
const MAX_IDS: usize = 4096;
/// How long one call on the bus may take.
const CALL_WAIT: Duration = Duration::from_secs(3);
/// How long the first [`available`] waits for the first look.
const FIRST_LOOK_WAIT: Duration = Duration::from_millis(300);
/// The helpers push what is in front at least every second (`front/`), and
/// what they said counts this long after their last push: a helper gone
/// quiet (GNOME turning extensions off at the lock screen, KWin restarted,
/// our script unloaded by another program, a KWin the script can't run on)
/// lets no key through after it.
const STALE: Duration = Duration::from_secs(3);
/// How often a watch looks whether its helper went quiet.
const TICK: Duration = Duration::from_secs(1);
/// A quiet helper is started again this often: GNOME's Watch (after the lock
/// screen, the extension is new) every time; KWin's script a few times per
/// KWin, since one it can't run would only fail again.
const REARM_EVERY: Duration = Duration::from_secs(2);
const KWIN_REARMS: u32 = 3;

/// The desktop's helper that tells us what is in front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Helper {
    /// KDE Plasma: a KWin script, loaded per session.
    KWin,
    /// GNOME: our extension, installed and on.
    Gnome,
}

impl Helper {
    fn bus_name(self) -> &'static str {
        match self {
            Self::KWin => KWIN,
            Self::Gnome => SHELL,
        }
    }
}

/// A window the helper listed. Its `Debug` leaves out the title.
#[derive(Clone, PartialEq, Eq)]
pub struct Window {
    /// Ours for it, for this process's life: never reused.
    pub id: isize,
    pub title: String,
    /// Its program (KWin's resource class, GNOME's WM class).
    pub app: String,
    pub pid: u32,
    /// Its sizes as the shell knows them (the client area, the frame), to
    /// match against what the screen-sharing portal shares.
    pub sizes: Vec<(u32, u32)>,
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window")
            .field("id", &self.id)
            .field("title", &"..")
            .field("app", &self.app)
            .field("pid", &self.pid)
            .field("sizes", &self.sizes)
            .finish()
    }
}

// ---- What the helper said ----

/// What the current watch was last told.
#[derive(Default)]
struct Told {
    /// The watch it belongs to; 0 for none.
    watch: u64,
    /// When the helper last pushed in this watch; `None` before its first
    /// push, and once its shell left the bus.
    heard: Option<Instant>,
    windows: Vec<Window>,
    /// The helper's id for each of ours, and ours for each of its.
    ids: HashMap<String, isize>,
    next_id: isize,
    /// Ours for the active window; 0 for none, -1 for one whose helper id
    /// was too long (never a window's).
    active: isize,
    /// The pointer is over the active window's client area.
    pointer: bool,
    /// Where the pointer is and the active window's client area, in the
    /// desktop's coordinates, if the helper says (an older GNOME extension
    /// doesn't).
    at: Option<(i32, i32)>,
    area: Option<Bounds>,
    /// Counts the changes of `at`: a guest's moves are cut from each new
    /// position (`couch_input::confine`), from when it came.
    moves: u64,
    moved_at: Option<Instant>,
    /// The lock screen is up, whatever the helper says: KWin keeps the
    /// active window while its lock screen takes the keys.
    locked: bool,
}

impl Told {
    fn id_for(&mut self, helper_id: &str) -> isize {
        if let Some(id) = self.ids.get(helper_id) {
            return *id;
        }
        if self.ids.len() >= MAX_IDS {
            // Forget the windows gone; ids still never come back.
            let (windows, active) = (&self.windows, self.active);
            self.ids
                .retain(|_, id| *id == active || windows.iter().any(|w| w.id == *id));
        }
        self.next_id += 1;
        let id = self.next_id;
        self.ids.insert(helper_id.to_owned(), id);
        id
    }

    /// Takes a pushed list in.
    fn windows(&mut self, pushed: Vec<Pushed>) {
        let windows = pushed
            .into_iter()
            .take(MAX_WINDOWS)
            .filter(|w| !w.id.is_empty() && w.id.len() <= MAX_HELPER_ID)
            .map(|w| Window {
                id: self.id_for(&w.id),
                title: cut(w.title),
                app: cut(w.app),
                pid: w.pid,
                sizes: w
                    .sizes
                    .into_iter()
                    .take(4)
                    .map(|[w, h]| (dimension(w), dimension(h)))
                    .collect(),
            })
            .collect();
        // Only what is in front keeps a helper counted (each push of it is
        // the heartbeat): a list alone never keeps an old front standing.
        self.windows = windows;
    }

    /// Takes the active window and the pointer in. An active window never
    /// listed (a dialog, a panel) gets an id too: never the game's.
    fn front(&mut self, pushed: PushedFront, now: Instant) {
        self.active = if pushed.active.is_empty() {
            0
        } else if pushed.active.len() > MAX_HELPER_ID {
            -1
        } else {
            self.id_for(&pushed.active)
        };
        self.pointer = pushed.pointer;
        let at = pushed
            .at
            .and_then(|[x, y]| Some((coordinate(x)?, coordinate(y)?)));
        if at.is_some() && at != self.at {
            self.moves += 1;
            self.moved_at = Some(now);
        }
        self.at = at;
        self.area = pushed.area.and_then(|[x, y, width, height]| {
            Some(Bounds {
                x: coordinate(x)?,
                y: coordinate(y)?,
                width: coordinate(width)?.max(0),
                height: coordinate(height)?.max(0),
            })
        });
        self.heard = Some(now);
    }

    /// The helper pushed within [`STALE`].
    fn live(&self, now: Instant) -> bool {
        self.heard
            .is_some_and(|heard| now.saturating_duration_since(heard) < STALE)
    }

    fn focus(&self, window: isize, now: Instant) -> (bool, bool) {
        let foreground = self.live(now) && !self.locked && window > 0 && self.active == window;
        (foreground, foreground && self.pointer)
    }

    /// Where the pointer is, while `window` is in front and the helper says.
    fn pointer(&self, window: isize, now: Instant) -> Option<PointerReport> {
        if !self.focus(window, now).0 {
            return None;
        }
        let ((x, y), area) = (self.at?, self.area?);
        Some(PointerReport {
            x,
            y,
            area,
            changes: self.moves,
            came: self.moved_at,
            told: self.heard,
        })
    }

    /// The watch's helper said nothing yet, or no longer counts.
    fn forget(&mut self) {
        self.heard = None;
        self.active = 0;
        self.pointer = false;
        self.at = None;
        self.area = None;
    }
}

/// A push, read before anything is taken in.
enum Push {
    Windows(Vec<Pushed>),
    Front(PushedFront),
}

fn cut(mut text: String) -> String {
    if text.len() > MAX_TEXT {
        let mut end = MAX_TEXT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// A position or size the helper pushed, in whole pixels; `None` for one
/// no screen has.
fn coordinate(value: f64) -> Option<i32> {
    const FAR: f64 = 1_000_000.0;
    (value.is_finite() && value.abs() <= FAR).then(|| value.round() as i32)
}

fn dimension(value: f64) -> u32 {
    if value.is_finite() && value > 0.0 {
        value.round().min(f64::from(u16::MAX)) as u32
    } else {
        0
    }
}

#[derive(Deserialize)]
struct Pushed {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    app: String,
    #[serde(default)]
    pid: u32,
    #[serde(default)]
    sizes: Vec<[f64; 2]>,
}

#[derive(Deserialize)]
struct PushedFront {
    active: String,
    #[serde(default)]
    pointer: bool,
    /// The pointer's position.
    #[serde(default)]
    at: Option<[f64; 2]>,
    /// The active window's client area: x, y, width, height.
    #[serde(default)]
    area: Option<[f64; 4]>,
}

fn told() -> std::sync::MutexGuard<'static, Told> {
    static TOLD: OnceLock<Mutex<Told>> = OnceLock::new();
    TOLD.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Whether the helper speaks in the current watch: it pushed within the
/// last few seconds.
pub fn live() -> bool {
    told().live(Instant::now())
}

/// The windows the helper last listed, while it speaks.
pub fn windows() -> Vec<Window> {
    let told = told();
    if told.live(Instant::now()) {
        told.windows.clone()
    } else {
        Vec::new()
    }
}

/// Whether `window` is in front, and the pointer over it, as last told:
/// neither while nobody watches or the helper is quiet.
pub fn focus(window: isize) -> (bool, bool) {
    told().focus(window, Instant::now())
}

/// Where the pointer is and `window`'s client area, as last told, while
/// `window` is in front and the helper says: what a guest's pointer is kept
/// in (`kbm`). Costs a lock, never the bus.
pub fn pointer(window: isize) -> Option<PointerReport> {
    told().pointer(window, Instant::now())
}

/// Whether `window` is still listed. True while the helper is quiet (a
/// choice isn't dropped for that; [`focus`] keeps keys off meanwhile).
pub fn exists(window: isize) -> bool {
    let told = told();
    !told.live(Instant::now()) || told.windows.iter().any(|w| w.id == window)
}

/// A folder of this user's alone for the KWin script when there is no
/// profile (`couch-cli`): `$XDG_RUNTIME_DIR`. Never a shared temporary
/// folder's fixed name.
pub fn private_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute() && dir.is_dir())
}

/// Whether a push from `sender` with `token` counts: from the owner of the
/// helper's bus name as last known, with this watch's token. Never from
/// anyone else, whatever it sends.
fn trusted(sender: Option<&str>, owner: Option<&str>, token: &str, expected: &str) -> bool {
    let (Some(sender), Some(owner)) = (sender, owner) else {
        return false;
    };
    !expected.is_empty() && sender == owner && token == expected
}

// ---- Whether a helper is there ----

/// The last look: 0 not yet, 1 none, 2 KWin, 3 GNOME's extension.
static AVAILABLE: AtomicU8 = AtomicU8::new(0);

/// The helper this desktop has, as last looked (KWin's scripting, or our
/// GNOME extension on); looks again in the background. The first call
/// waits a moment for the first look.
pub fn available() -> Option<Helper> {
    static LOOK: OnceLock<Option<Arc<Notify>>> = OnceLock::new();
    let look = LOOK.get_or_init(|| {
        let runtime = match crate::portal::runtime() {
            Ok(runtime) => runtime,
            Err(e) => {
                tracing::warn!("what is in front can't be known: {e}");
                return None;
            }
        };
        let look = Arc::new(Notify::new());
        runtime.spawn(keep_looking(look.clone()));
        Some(look)
    });
    let Some(look) = look else {
        return None;
    };
    look.notify_one();
    if AVAILABLE.load(Ordering::Acquire) == 0 {
        let until = std::time::Instant::now() + FIRST_LOOK_WAIT;
        while AVAILABLE.load(Ordering::Acquire) == 0 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    match AVAILABLE.load(Ordering::Acquire) {
        2 => Some(Helper::KWin),
        3 => Some(Helper::Gnome),
        _ => None,
    }
}

async fn keep_looking(look: Arc<Notify>) {
    let mut bus: Option<zbus::Connection> = None;
    loop {
        let found = tokio::time::timeout(CALL_WAIT, async {
            if bus.is_none() {
                bus = Some(zbus::Connection::session().await?);
            }
            let bus = bus.as_ref().expect("connected above");
            Ok::<_, zbus::Error>(find_helper(bus).await)
        })
        .await;
        let value = match found {
            Ok(Ok(Some(Helper::KWin))) => 2,
            Ok(Ok(Some(Helper::Gnome))) => 3,
            Ok(Ok(None)) => 1,
            Ok(Err(e)) => {
                tracing::debug!("no session bus to look for a helper on: {e}");
                bus = None;
                1
            }
            Err(_) => 1,
        };
        AVAILABLE.store(value, Ordering::Release);
        look.notified().await;
        // Looks pile into one, at most a few a second.
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn has_owner(bus: &zbus::Connection, name: &str) -> bool {
    let Ok(dbus) = DBusProxy::new(bus).await else {
        return false;
    };
    let Ok(name) = BusName::try_from(name) else {
        return false;
    };
    dbus.name_has_owner(name).await.unwrap_or(false)
}

async fn owner(bus: &zbus::Connection, name: &str) -> Option<String> {
    let dbus = DBusProxy::new(bus).await.ok()?;
    let name = BusName::try_from(name).ok()?;
    dbus.get_name_owner(name)
        .await
        .ok()
        .map(|owner| owner.to_string())
}

/// GNOME's Shell with our extension on, or KWin.
async fn find_helper(bus: &zbus::Connection) -> Option<Helper> {
    if has_owner(bus, SHELL).await {
        return (gnome::look(bus).await == gnome::ExtensionState::Ready).then_some(Helper::Gnome);
    }
    has_owner(bus, KWIN).await.then_some(Helper::KWin)
}

// ---- Watching ----

/// The helper runs, telling us what is in front, until this drops: the
/// KWin script unloaded, or our watch on GNOME's extension ended.
pub struct Watch {
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

static WATCHES: AtomicU64 = AtomicU64::new(0);

/// Starts the desktop's helper for a session: returns at once, the helper
/// speaking a moment later ([`live`]). `dir` holds the KWin script (the
/// profile's folder, private to the user), `name` tells this profile's apart
/// (the script's plugin name). One watch at a time: a new one replaces the
/// last.
pub fn watch(dir: &Path, name: &str) -> Result<Watch, String> {
    let runtime = crate::portal::runtime()?;
    let watch = WATCHES.fetch_add(1, Ordering::AcqRel) + 1;
    {
        let mut told = told();
        let next_id = told.next_id;
        let ids = std::mem::take(&mut told.ids);
        *told = Told {
            watch,
            next_id,
            ids,
            ..Told::default()
        };
    }
    let mut token = [0u8; 16];
    getrandom::fill(&mut token).map_err(|e| format!("no token for the helper: {e}"))?;
    let token: String = token.iter().map(|b| format!("{b:02x}")).collect();
    let (stop_tx, stop_rx) = oneshot::channel();
    let setup = Setup {
        watch,
        token,
        script: dir.join("front-kwin.js"),
        plugin: format!("couch-coapp-front-{}", plugin_part(name)),
    };
    runtime.spawn(async move {
        if let Err(e) = run(setup, stop_rx).await {
            tracing::warn!("what is in front can't be known: {e}");
        }
        let mut told = told();
        if told.watch == watch {
            told.watch = 0;
            told.forget();
            told.windows.clear();
        }
    });
    Ok(Watch {
        stop: Some(stop_tx),
    })
}

/// A plugin name's part from a profile's name: letters, digits, `-`, `_`.
fn plugin_part(name: &str) -> String {
    let part: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(48)
        .collect();
    if part.is_empty() {
        "default".into()
    } else {
        part
    }
}

#[derive(Clone)]
struct Setup {
    watch: u64,
    token: String,
    script: PathBuf,
    plugin: String,
}

/// One helper at a time in this process: a watch's KWin script is unloaded,
/// and its file removed, before the next one's is written and loaded under
/// the same name.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One watch: our own connection (its unique name is where the helper
/// pushes), the helper started, pushes taken until `stop` (or a newer
/// watch), a quiet helper started again, then the helper stopped and the
/// connection closed.
///
/// Nothing awaits the bus between two messages but a reply's send, and
/// that with a time limit: zbus's reader waits while a stream's queue is
/// full, so a call answered on this connection while its messages aren't
/// read could wait for ever.
async fn run(setup: Setup, mut stop: oneshot::Receiver<()>) -> Result<(), String> {
    let _one = tokio::select! {
        _ = &mut stop => return Ok(()),
        one = ONE_AT_A_TIME.lock() => one,
    };
    if told().watch != setup.watch {
        return Ok(());
    }
    let bus = tokio::time::timeout(CALL_WAIT, zbus::Connection::session())
        .await
        .map_err(|_| "the session bus didn't answer".to_string())?
        .map_err(|e| format!("no session bus: {e}"))?;
    let service = bus
        .unique_name()
        .map(ToString::to_string)
        .ok_or("no name on the session bus")?;
    let helper = tokio::time::timeout(CALL_WAIT * 2, find_helper(&bus))
        .await
        .ok()
        .flatten()
        .ok_or("neither KDE's KWin nor Couch Co-App's GNOME extension is running")?;
    let shell = helper.bus_name();
    // Who owns the shell's name, followed from the bus's own signals (only
    // the bus sends as `org.freedesktop.DBus`), read before the helper
    // starts: its first pushes come at once.
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender("org.freedesktop.DBus")
        .and_then(|b| b.interface("org.freedesktop.DBus"))
        .and_then(|b| b.member("NameOwnerChanged"))
        .and_then(|b| b.add_arg(shell))
        .map_err(|e| e.to_string())?
        .build();
    let mut owner_changes = rule_stream(&bus, rule).await?;
    // The lock screen, read beside the helper: KWin keeps its active window
    // while the lock screen takes the keys, so the helper never says.
    // `org.freedesktop.ScreenSaver` signals the change at once (KDE's, and
    // GNOME's where it has one); logind's `LockedHint` backs it up, read
    // each second without waiting (`session_lock`).
    let saver_rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(SCREEN_SAVER)
        .and_then(|b| b.interface(SCREEN_SAVER))
        .and_then(|b| b.member("ActiveChanged"))
        .map_err(|e| e.to_string())?
        .build();
    let mut saver_changes = rule_stream(&bus, saver_rule).await?;
    let mut saver_active = screen_saver_active(&bus).await;
    let mut messages = zbus::MessageStream::from(&bus);
    let mut owner = tokio::time::timeout(CALL_WAIT, owner(&bus, shell))
        .await
        .ok()
        .flatten();
    // Counts the shell's changes, so an owner a re-arm looked up isn't taken
    // over a change that came meanwhile.
    let mut owner_changes_seen = 0u64;
    set_locked(&setup, saver_active || crate::session_lock::locked());
    if let Err(e) = start(&bus, helper, &setup, &service).await {
        stop_helper(&bus, helper, &setup).await;
        return Err(e);
    }
    tracing::info!(?helper, "the desktop tells us which window is in front");
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut rearm: Option<Rearm> = None;
    let mut next_rearm = Instant::now() + STALE;
    let mut rearms = 0u32;
    let mut was_live = false;
    loop {
        tokio::select! {
            _ = &mut stop => break,
            message = messages.next() => match message {
                Some(Ok(message)) => {
                    if let Some(reply) = take(&message, owner.as_deref(), &setup) {
                        let _ = tokio::time::timeout(CALL_WAIT, bus.send(&reply)).await;
                    }
                }
                Some(Err(_)) => {}
                None => break,
            },
            change = owner_changes.next() => match change {
                Some(Ok(change)) => {
                    if let Some(new) = owner_changed(&change, shell) {
                        // Another shell (or none): nothing it said counts,
                        // its ids name nothing (GNOME's restart from 1), and
                        // its helper starts again.
                        tracing::info!(?helper, "the desktop's shell changed: what is in front waits for it");
                        owner = new;
                        owner_changes_seen += 1;
                        let mut told = told();
                        if told.watch == setup.watch {
                            told.forget();
                            told.ids.clear();
                        }
                        next_rearm = Instant::now();
                        rearms = 0;
                    }
                }
                Some(Err(_)) => {}
                None => break,
            },
            change = saver_changes.next() => match change {
                Some(Ok(change)) => {
                    if let Some(active) = saver_changed(&change) {
                        saver_active = active;
                        set_locked(&setup, saver_active || crate::session_lock::locked());
                    }
                }
                Some(Err(_)) => {}
                None => break,
            },
            _ = tick.tick() => {
                let now = Instant::now();
                let live = {
                    let mut told = told();
                    if told.watch != setup.watch {
                        // A newer watch replaced this one.
                        break;
                    }
                    told.locked = saver_active || crate::session_lock::locked();
                    told.live(now)
                };
                if let Some(task) = rearm.take_if(|r| r.task.is_finished()) {
                    match task.task.await {
                        Ok((found, result)) => {
                            if owner.is_none() && task.seen == owner_changes_seen {
                                owner = found;
                            }
                            if let Err(e) = result {
                                tracing::debug!("the desktop's helper didn't start again: {e}");
                            }
                        }
                        Err(e) => tracing::debug!("the desktop's helper didn't start again: {e}"),
                    }
                }
                if live {
                    rearms = 0;
                } else {
                    if was_live {
                        tracing::warn!(?helper, "the desktop's helper went quiet: guests' keys wait for it");
                    }
                    let may = match helper {
                        Helper::Gnome => true,
                        Helper::KWin => rearms < KWIN_REARMS,
                    };
                    if rearm.is_none() && may && now >= next_rearm {
                        rearms += 1;
                        if helper == Helper::KWin && rearms == KWIN_REARMS {
                            tracing::warn!("KWin hasn't run our script: this KWin may be too old for it");
                        }
                        next_rearm = now + REARM_EVERY;
                        let (bus, setup, service) = (bus.clone(), setup.clone(), service.clone());
                        // The shell's owner too, if the first look missed it.
                        let look = owner.is_none();
                        rearm = Some(Rearm {
                            seen: owner_changes_seen,
                            task: tokio::spawn(async move {
                                let found = if look {
                                    tokio::time::timeout(CALL_WAIT, crate::front::owner(&bus, shell))
                                        .await
                                        .ok()
                                        .flatten()
                                } else {
                                    None
                                };
                                (found, start(&bus, helper, &setup, &service).await)
                            }),
                        });
                    }
                }
                was_live = live;
            }
        }
    }
    if let Some(rearm) = rearm.take() {
        // Ended before the helper is stopped, so it can't load a script
        // after the unload, nor write the file after its removal.
        rearm.task.abort();
        let _ = rearm.task.await;
    }
    stop_helper(&bus, helper, &setup).await;
    Ok(())
}

/// A helper being started again: the shell's owner it looked up, if asked
/// to, and how the start went.
struct Rearm {
    /// The shell's changes seen when it began.
    seen: u64,
    task: tokio::task::JoinHandle<(Option<String>, Result<(), String>)>,
}

const SCREEN_SAVER: &str = "org.freedesktop.ScreenSaver";

/// A stream of the messages `rule` matches, registered with the bus.
async fn rule_stream(
    bus: &zbus::Connection,
    rule: MatchRule<'_>,
) -> Result<zbus::MessageStream, String> {
    tokio::time::timeout(
        CALL_WAIT,
        zbus::MessageStream::for_match_rule(rule, bus, Some(16)),
    )
    .await
    .map_err(|_| "the session bus didn't answer".to_string())?
    .map_err(|e| e.to_string())
}

/// Whether the desktop's screen saver says its lock screen is up; false
/// where there's none to ask.
async fn screen_saver_active(bus: &zbus::Connection) -> bool {
    let Ok(reply) = call(
        bus,
        SCREEN_SAVER,
        "/org/freedesktop/ScreenSaver",
        SCREEN_SAVER,
        "GetActive",
        &(),
    )
    .await
    else {
        return false;
    };
    reply.body().deserialize::<bool>().unwrap_or(false)
}

/// The lock screen's state from an `ActiveChanged` signal; `None` for any
/// other message.
fn saver_changed(message: &zbus::Message) -> Option<bool> {
    let header = message.header();
    if header.message_type() != MessageType::Signal
        || header.interface().map(|i| i.as_str()) != Some(SCREEN_SAVER)
        || header.member().map(|m| m.as_str()) != Some("ActiveChanged")
    {
        return None;
    }
    message.body().deserialize::<bool>().ok()
}

fn set_locked(setup: &Setup, locked: bool) {
    let mut told = told();
    if told.watch == setup.watch {
        if locked && !told.locked {
            tracing::info!("the lock screen is up: guests' keys wait");
        }
        told.locked = locked;
    }
}

/// Starts the helper, or again: the KWin script (re)loaded, or our watch on
/// GNOME's extension asked for (again).
async fn start(
    bus: &zbus::Connection,
    helper: Helper,
    setup: &Setup,
    service: &str,
) -> Result<(), String> {
    match helper {
        Helper::KWin => kwin::load(bus, setup, service).await,
        Helper::Gnome => gnome::watch(bus, &setup.token).await,
    }
}

async fn stop_helper(bus: &zbus::Connection, helper: Helper, setup: &Setup) {
    match helper {
        Helper::KWin => {
            if let Err(e) = kwin::unload(bus, &setup.plugin).await {
                tracing::warn!("the KWin script couldn't be unloaded: {e}");
            }
            let _ = std::fs::remove_file(&setup.script);
        }
        Helper::Gnome => {
            let _ = gnome::unwatch(bus).await;
        }
    }
}

/// The new owner of `shell` from a `NameOwnerChanged` signal for it (`None`
/// inside: nobody owns it); `None` for any other message.
fn owner_changed(message: &zbus::Message, shell: &str) -> Option<Option<String>> {
    let header = message.header();
    if header.message_type() != MessageType::Signal
        || header.sender().map(|s| s.as_str()) != Some("org.freedesktop.DBus")
        || header.member().map(|m| m.as_str()) != Some("NameOwnerChanged")
    {
        return None;
    }
    let (name, _old, new) = message
        .body()
        .deserialize::<(String, String, String)>()
        .ok()?;
    (name == shell).then(|| (!new.is_empty()).then_some(new))
}

/// One message: a push to our object, taken only from the helper's shell
/// (`owner`, as last known) with our token. Returns the answer to send
/// (GNOME's waits for one). Reads only what it was sent: never the bus.
fn take(message: &zbus::Message, owner: Option<&str>, setup: &Setup) -> Option<zbus::Message> {
    let header = message.header();
    if header.message_type() != MessageType::MethodCall
        || header.path().map(|p| p.as_str()) != Some(PATH)
        || header.interface().map(|i| i.as_str()) != Some(IFACE)
    {
        return None;
    }
    let member = header.member().map(|m| m.as_str().to_owned());
    let sender = header.sender().map(|s| s.as_str().to_owned());
    // The sender first: from anyone else, nothing is even read.
    let push = (sender.is_some() && sender.as_deref() == owner)
        .then(|| message.body().deserialize::<(String, String)>().ok())
        .flatten()
        .filter(|(token, json)| {
            json.len() <= MAX_PUSH && trusted(sender.as_deref(), owner, token, &setup.token)
        })
        .and_then(|(_, json)| match member.as_deref() {
            Some("Windows") => serde_json::from_str(&json).ok().map(Push::Windows),
            Some("Front") => serde_json::from_str(&json).ok().map(Push::Front),
            _ => None,
        });
    let accepted = push.is_some_and(|push| {
        let now = Instant::now();
        let mut told = told();
        if told.watch != setup.watch {
            return false;
        }
        match push {
            Push::Windows(windows) => told.windows(windows),
            Push::Front(front) => told.front(front, now),
        }
        true
    });
    if !accepted {
        tracing::debug!(?member, "a push about the front was ignored");
    }
    let reply = if accepted {
        zbus::Message::method_return(&header).and_then(|b| b.build(&()))
    } else {
        zbus::Message::error(&header, "com.couchcoapp.Front.Error.Refused")
            .and_then(|b| b.build(&()))
    };
    reply.ok()
}

async fn call(
    bus: &zbus::Connection,
    destination: &str,
    path: &str,
    interface: &str,
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> Result<zbus::Message, String> {
    tokio::time::timeout(
        CALL_WAIT,
        bus.call_method(Some(destination), path, Some(interface), method, body),
    )
    .await
    .map_err(|_| format!("{destination} didn't answer {method}"))?
    .map_err(|e| format!("{method}: {e}"))
}

/// Runs `future` on the portals' runtime and waits for it, at most
/// `CALL_WAIT` and a moment: for the few calls made from a thread that may
/// wait (Settings → Drivers).
fn block_on<T: Send + 'static>(
    future: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    let runtime = crate::portal::runtime()?;
    let (tx, rx) = std::sync::mpsc::channel();
    runtime.spawn(async move {
        let _ = tx.send(future.await);
    });
    rx.recv_timeout(CALL_WAIT * 2)
        .map_err(|_| "the session bus didn't answer".to_string())?
}

// ---- KDE ----

mod kwin {
    use super::*;

    const SCRIPTING: &str = "/Scripting";
    const SCRIPTING_IFACE: &str = "org.kde.kwin.Scripting";
    const SCRIPT_IFACE: &str = "org.kde.kwin.Script";

    /// The script for this session: our connection's name and the token
    /// filled in. Both are bus-name and hex characters, nothing a script
    /// string needs escaped.
    pub(super) fn script(service: &str, token: &str) -> String {
        KWIN_SCRIPT
            .replace("@SERVICE@", service)
            .replace("@TOKEN@", token)
    }

    /// Writes the script, unloads any left by a crash under the same name,
    /// then loads and runs it.
    pub(super) async fn load(
        bus: &zbus::Connection,
        setup: &Setup,
        service: &str,
    ) -> Result<(), String> {
        write_private(&setup.script, script(service, &setup.token).as_bytes())
            .map_err(|e| format!("the KWin script couldn't be written: {e}"))?;
        let _ = unload(bus, &setup.plugin).await;
        let path = setup.script.to_string_lossy().into_owned();
        let reply = call(
            bus,
            KWIN,
            SCRIPTING,
            SCRIPTING_IFACE,
            "loadScript",
            &(path, setup.plugin.as_str()),
        )
        .await?;
        let id: i32 = reply
            .body()
            .deserialize()
            .map_err(|e| format!("loadScript: {e}"))?;
        if id < 0 {
            return Err("KWin refused the script".into());
        }
        call(
            bus,
            KWIN,
            &format!("{SCRIPTING}/Script{id}"),
            SCRIPT_IFACE,
            "run",
            &(),
        )
        .await?;
        Ok(())
    }

    pub(super) async fn unload(bus: &zbus::Connection, plugin: &str) -> Result<(), String> {
        call(
            bus,
            KWIN,
            SCRIPTING,
            SCRIPTING_IFACE,
            "unloadScript",
            &(plugin,),
        )
        .await
        .map(|_| ())
    }
}

/// Writes `bytes` to a new file at `path`, readable by this user alone:
/// what was there removed first, and the file made afresh without following
/// a link, so a file or link someone else left there (in a folder others
/// may write) is never written through, and fails the write instead.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    file.write_all(bytes)
}

// ---- GNOME ----

/// Our GNOME extension: installed by Settings → Drivers, which also turns it
/// on once GNOME knows it (after logging out and in).
pub mod gnome {
    use super::*;

    const SHELL_PATH: &str = "/org/gnome/Shell";
    const EXTENSIONS_IFACE: &str = "org.gnome.Shell.Extensions";

    /// Where our extension stands.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ExtensionState {
        /// Not installed.
        Missing,
        /// An older version of it is installed: Install updates it.
        Outdated,
        /// Installed, but GNOME reads extensions only when you log in: it
        /// doesn't know ours yet, or still runs an older version of it.
        NeedsLogout,
        /// Installed and known, but turned off.
        Off,
        /// GNOME's extensions are all turned off (the Extensions app).
        ExtensionsOff,
        /// GNOME can't run it (another GNOME version, or it failed).
        Failed,
        Ready,
    }

    fn folder() -> Option<PathBuf> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })?;
        Some(data.join("gnome-shell/extensions").join(EXTENSION_UUID))
    }

    /// Whether our files, as this build has them, are in place: `Some(true)`;
    /// others of ours (an older version): `Some(false)`; none: `None`.
    fn installed() -> Option<bool> {
        let folder = folder()?;
        let metadata = std::fs::read_to_string(folder.join("metadata.json")).ok()?;
        Some(
            metadata == EXTENSION_METADATA
                && std::fs::read_to_string(folder.join("extension.js"))
                    .is_ok_and(|j| j == EXTENSION_JS),
        )
    }

    /// Our extension's state; `None` off GNOME. Blocks briefly.
    pub fn state() -> Option<ExtensionState> {
        block_on(async {
            let bus = zbus::Connection::session()
                .await
                .map_err(|e| e.to_string())?;
            if !has_owner(&bus, SHELL).await {
                return Ok(None);
            }
            Ok(Some(look(&bus).await))
        })
        .unwrap_or_else(|e| {
            tracing::debug!("GNOME's extensions can't be read: {e}");
            None
        })
    }

    pub(super) async fn look(bus: &zbus::Connection) -> ExtensionState {
        match installed() {
            None => return ExtensionState::Missing,
            Some(false) => return ExtensionState::Outdated,
            Some(true) => {}
        }
        if !user_extensions_on(bus).await {
            return ExtensionState::ExtensionsOff;
        }
        match info(bus).await {
            // Unknown to the Shell: installed since it started.
            None => ExtensionState::NeedsLogout,
            // The Shell still runs the version it read at log-in.
            Some((_, version)) if version != Some(EXTENSION_VERSION) => ExtensionState::NeedsLogout,
            Some((state, _)) => match state as u32 {
                1 => ExtensionState::Ready,
                // Off, being turned off, or read but not yet on.
                2 | 6 | 7 => ExtensionState::Off,
                // Being turned on.
                8 => ExtensionState::Off,
                _ => ExtensionState::Failed,
            },
        }
    }

    /// The Shell's state for our extension (1 on, 2 off, 3 failed, 4 out of
    /// date, 6 read, ...) and the version it read; `None` if it doesn't know
    /// it.
    async fn info(bus: &zbus::Connection) -> Option<(f64, Option<f64>)> {
        let reply = call(
            bus,
            SHELL,
            SHELL_PATH,
            EXTENSIONS_IFACE,
            "GetExtensionInfo",
            &(EXTENSION_UUID,),
        )
        .await
        .ok()?;
        let info: HashMap<String, OwnedValue> = reply.body().deserialize().ok()?;
        let state = f64::try_from(info.get("state")?).ok()?;
        let version = info.get("version").and_then(|v| f64::try_from(v).ok());
        Some((state, version))
    }

    async fn user_extensions_on(bus: &zbus::Connection) -> bool {
        let reply = call(
            bus,
            SHELL,
            SHELL_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            &(EXTENSIONS_IFACE, "UserExtensionsEnabled"),
        )
        .await;
        let Ok(reply) = reply else { return true };
        reply
            .body()
            .deserialize::<OwnedValue>()
            .ok()
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(true)
    }

    /// Puts our files in place (always this build's), then turns the
    /// extension on if GNOME knows it already. Returns where it stands.
    pub fn install() -> Result<ExtensionState, String> {
        let folder = folder().ok_or("no home folder")?;
        std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        for (name, text) in [
            ("metadata.json", EXTENSION_METADATA),
            ("extension.js", EXTENSION_JS),
        ] {
            let path = folder.join(name);
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        block_on(async {
            let bus = zbus::Connection::session()
                .await
                .map_err(|e| e.to_string())?;
            let state = look(&bus).await;
            if state != ExtensionState::Off {
                return Ok(state);
            }
            call(
                &bus,
                SHELL,
                SHELL_PATH,
                EXTENSIONS_IFACE,
                "EnableExtension",
                &(EXTENSION_UUID,),
            )
            .await?;
            Ok(look(&bus).await)
        })
    }

    pub(super) async fn watch(bus: &zbus::Connection, token: &str) -> Result<(), String> {
        call(bus, SHELL, PATH, IFACE, "Watch", &(token,))
            .await
            .map(|_| ())
    }

    pub(super) async fn unwatch(bus: &zbus::Connection) -> Result<(), String> {
        call(bus, SHELL, PATH, IFACE, "Unwatch", &())
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_counts_only_from_the_shell_with_our_token() {
        let ok = |s, o, t| trusted(s, o, t, "abc");
        assert!(ok(Some(":1.5"), Some(":1.5"), "abc"));
        // Anyone else on the bus, with or without the token.
        assert!(!ok(Some(":1.9"), Some(":1.5"), "abc"));
        // The shell, without our token (another session's script).
        assert!(!ok(Some(":1.5"), Some(":1.5"), "abd"));
        assert!(!ok(Some(":1.5"), Some(":1.5"), ""));
        // No shell running, or no sender.
        assert!(!ok(Some(":1.5"), None, "abc"));
        assert!(!ok(None, Some(":1.5"), "abc"));
        // No watch.
        assert!(!trusted(Some(":1.5"), Some(":1.5"), "", ""));
    }

    fn push(told: &mut Told, json: &str) {
        told.windows(serde_json::from_str(json).unwrap());
    }

    fn front(told: &mut Told, json: &str) {
        told.front(serde_json::from_str(json).unwrap(), Instant::now());
    }

    #[test]
    fn a_quiet_helper_lets_nothing_through() {
        let mut told = Told::default();
        push(&mut told, r#"[{"id":"a"}]"#);
        front(&mut told, r#"{"active":"a","pointer":true}"#);
        let game = told.windows[0].id;
        let heard = told.heard.unwrap();
        assert_eq!(told.focus(game, heard + STALE / 2), (true, true));
        // No push for a while (the lock screen, KWin gone): nothing in front,
        // and the choice of game isn't dropped for it.
        assert_eq!(told.focus(game, heard + STALE), (false, false));
        assert!(!told.live(heard + STALE));
        // Its shell left the bus: nothing at once.
        told.forget();
        assert_eq!(told.focus(game, heard), (false, false));
        // It speaks again: the same window, the same id.
        push(&mut told, r#"[{"id":"a"}]"#);
        front(&mut told, r#"{"active":"a","pointer":false}"#);
        assert_eq!(told.windows[0].id, game);
        assert_eq!(told.focus(game, Instant::now()), (true, false));
        // The lock screen up, the helper still saying the game (KWin keeps
        // its active window): nothing.
        told.locked = true;
        front(&mut told, r#"{"active":"a","pointer":true}"#);
        assert_eq!(told.focus(game, Instant::now()), (false, false));
        told.locked = false;
        assert_eq!(told.focus(game, Instant::now()), (true, true));
        // A list alone keeps nothing alive: only what is in front does.
        let mut quiet = Told::default();
        push(&mut quiet, r#"[{"id":"a"}]"#);
        assert!(!quiet.live(Instant::now()));
    }

    #[test]
    fn the_pointer_is_told_only_for_the_window_in_front() {
        let mut told = Told::default();
        push(&mut told, r#"[{"id":"a"},{"id":"b"}]"#);
        let (game, other) = (told.windows[0].id, told.windows[1].id);
        // An older GNOME extension says nothing of where the pointer is.
        front(&mut told, r#"{"active":"a","pointer":true}"#);
        assert_eq!(told.pointer(game, Instant::now()), None);
        let area = Bounds {
            x: 10,
            y: 40,
            width: 800,
            height: 600,
        };
        front(
            &mut told,
            r#"{"active":"a","pointer":true,"at":[100.4,200.6],"area":[10,40,800,600]}"#,
        );
        let first = told.pointer(game, Instant::now()).unwrap();
        assert_eq!((first.x, first.y, first.area), (100, 201, area));
        assert_eq!(told.pointer(other, Instant::now()), None);
        // The heartbeat with the same position isn't a new one; a move is.
        front(
            &mut told,
            r#"{"active":"a","pointer":true,"at":[100,201],"area":[10,40,800,600]}"#,
        );
        assert_eq!(
            told.pointer(game, Instant::now()).unwrap().changes,
            first.changes
        );
        front(
            &mut told,
            r#"{"active":"a","pointer":false,"at":[5,20],"area":[10,40,800,600]}"#,
        );
        let moved = told.pointer(game, Instant::now()).unwrap();
        assert_ne!(moved.changes, first.changes);
        assert_eq!((moved.x, moved.y), (5, 20));
        // Nothing while another window is in front, the helper is quiet, or
        // for a position no screen has.
        front(&mut told, r#"{"active":"b","at":[5,20],"area":[0,0,9,9]}"#);
        assert_eq!(told.pointer(game, Instant::now()), None);
        front(
            &mut told,
            r#"{"active":"a","at":[5,20],"area":[10,40,800,600]}"#,
        );
        let heard = told.heard.unwrap();
        assert_eq!(told.pointer(game, heard + STALE), None);
        front(
            &mut told,
            r#"{"active":"a","at":[1e300,20],"area":[10,40,800,600]}"#,
        );
        assert_eq!(told.pointer(game, Instant::now()), None);
        told.forget();
        assert_eq!(told.pointer(game, Instant::now()), None);
    }

    #[test]
    fn long_or_many_helper_ids_are_kept_in_bounds() {
        let mut told = Told::default();
        let long = "x".repeat(MAX_HELPER_ID + 1);
        push(&mut told, &format!(r#"[{{"id":"{long}"}},{{"id":"a"}}]"#));
        assert_eq!(told.windows.len(), 1, "a window with a long id is left out");
        let game = told.windows[0].id;
        // A long active id is a window never listed: never the game.
        front(
            &mut told,
            &format!(r#"{{"active":"{long}","pointer":true}}"#),
        );
        assert_eq!(told.active, -1);
        assert_eq!(told.focus(game, Instant::now()), (false, false));
        // Many windows come and go: the ids of those gone are forgotten,
        // the listed and active ones kept, and no id comes back.
        front(&mut told, r#"{"active":"a","pointer":true}"#);
        for i in 0..MAX_IDS + 10 {
            let _ = told.id_for(&format!("gone{i}"));
        }
        assert!(told.ids.len() <= MAX_IDS);
        assert_eq!(told.id_for("a"), game);
        let next = told.next_id;
        assert_eq!(told.id_for("gone0"), next + 1, "a forgotten id is new");
    }

    #[test]
    fn keys_reach_a_window_only_while_it_is_in_front() {
        let mut told = Told::default();
        // Nothing heard yet.
        assert_eq!(told.focus(1, Instant::now()), (false, false));
        push(
            &mut told,
            r#"[{"id":"{a}","title":"Game","app":"game","pid":7,"sizes":[[1280,720],[1282,750]]},
                {"id":"{b}","title":"Editor","app":"kate","pid":8,"sizes":[]}]"#,
        );
        let game = told.windows[0].id;
        let editor = told.windows[1].id;
        assert_eq!(told.windows[0].sizes, vec![(1280, 720), (1282, 750)]);
        assert_eq!(told.focus(game, Instant::now()), (false, false));
        front(&mut told, r#"{"active":"{a}","pointer":false}"#);
        assert_eq!(told.focus(game, Instant::now()), (true, false));
        front(&mut told, r#"{"active":"{a}","pointer":true}"#);
        assert_eq!(told.focus(game, Instant::now()), (true, true));
        assert_eq!(told.focus(editor, Instant::now()), (false, false));
        // A window never listed (a dialog) in front: not the game.
        front(&mut told, r#"{"active":"{c}","pointer":true}"#);
        assert_eq!(told.focus(game, Instant::now()), (false, false));
        // Nothing in front.
        front(&mut told, r#"{"active":"","pointer":true}"#);
        assert_eq!(told.focus(game, Instant::now()), (false, false));
        // No window is 0.
        assert_eq!(told.focus(0, Instant::now()), (false, false));
    }

    #[test]
    fn ids_stay_with_their_windows_and_are_never_reused() {
        let mut told = Told::default();
        push(&mut told, r#"[{"id":"a"},{"id":"b"}]"#);
        let (a, b) = (told.windows[0].id, told.windows[1].id);
        push(&mut told, r#"[{"id":"b"}]"#);
        assert_eq!(told.windows[0].id, b);
        push(&mut told, r#"[{"id":"c"},{"id":"a"}]"#);
        assert_eq!(told.windows[1].id, a);
        assert!(told.windows[0].id != a && told.windows[0].id != b);
    }

    #[test]
    fn pushes_are_cut_to_size() {
        let mut told = Told::default();
        let long = "é".repeat(MAX_TEXT);
        push(
            &mut told,
            &format!(
                r#"[{{"id":"a","title":"{long}","sizes":[[-1,1e12],[1,2],[3,4],[5,6],[7,8]]}},{{"id":""}}]"#
            ),
        );
        assert_eq!(told.windows.len(), 1, "a window without an id is left out");
        assert!(told.windows[0].title.len() <= MAX_TEXT);
        assert_eq!(told.windows[0].sizes.len(), 4);
        assert_eq!(told.windows[0].sizes[0], (0, u32::from(u16::MAX)));
    }

    #[test]
    fn the_kwin_script_names_us_and_our_token() {
        let script = kwin::script(":1.42", "00ff");
        assert!(script.contains(r#"const SERVICE = ":1.42";"#));
        assert!(script.contains(r#"const TOKEN = "00ff";"#));
        assert!(!script.contains('@'), "every placeholder filled");
        assert!(script.contains(PATH) && script.contains(IFACE));
    }

    #[test]
    fn the_extension_speaks_our_interface() {
        assert!(EXTENSION_JS.contains(PATH) || EXTENSION_JS.contains("/com/couchcoapp/Front"));
        assert!(EXTENSION_JS.contains(&format!("'{IFACE}'")));
        assert!(EXTENSION_METADATA.contains(EXTENSION_UUID));
        assert!(EXTENSION_METADATA.contains(&format!("\"version\": {EXTENSION_VERSION}")));
    }

    /// GNOME runs the extension it read at log-in, and the app tells an
    /// older one by its version alone: a change to `extension.js` without a
    /// new version would count as ready while GNOME still runs the old code
    /// (P18's review). Changed it? Raise `metadata.json`'s version and
    /// `EXTENSION_VERSION`, then put the new hash here.
    #[test]
    fn the_extensions_version_moves_with_its_code() {
        let code = EXTENSION_JS.replace('\r', "");
        let hash = code.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        assert_eq!((EXTENSION_VERSION, hash), (5.0, 0xed39_9fa7_a222_c2ae));
    }

    /// The whole path on a real bus, with a stand-in KWin: the script loaded
    /// and run, its pushes taken, a spoofed one refused, nothing in front once
    /// KWin leaves the bus and the script loaded again when it is back, the
    /// script unloaded when the watch drops. Run it under a private bus:
    /// `dbus-run-session -- cargo test -p couch-platform-linux front -- --ignored`.
    #[test]
    #[ignore = "needs a session bus of its own (dbus-run-session)"]
    fn a_stand_in_kwin_tells_us_and_nobody_else_does() {
        use std::sync::atomic::AtomicU32;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        // Once before loading (a crash's leftover), once at the end.
        let unloaded = Arc::new(AtomicU32::new(0));
        let kwin = rt.block_on(async {
            let kwin = zbus::Connection::session().await.unwrap();
            kwin.request_name(KWIN).await.unwrap();
            kwin
        });
        // The stand-in: answers Scripting, and once run, pushes as the
        // script would, from KWin's own name.
        let fake = kwin.clone();
        let unloaded_flag = unloaded.clone();
        rt.spawn(async move {
            let mut messages = zbus::MessageStream::from(&fake);
            let mut script = PathBuf::new();
            while let Some(Ok(message)) = messages.next().await {
                let header = message.header();
                if header.message_type() != MessageType::MethodCall {
                    continue;
                }
                let member = header.member().map(|m| m.as_str().to_owned());
                let reply = zbus::Message::method_return(&header).unwrap();
                match member.as_deref() {
                    Some("loadScript") => {
                        let (path, _name): (String, String) = message.body().deserialize().unwrap();
                        script = PathBuf::from(path);
                        fake.send(&reply.build(&(1i32,)).unwrap()).await.unwrap();
                    }
                    Some("run") => {
                        fake.send(&reply.build(&()).unwrap()).await.unwrap();
                        let text = std::fs::read_to_string(&script).unwrap();
                        let value = |key: &str| {
                            let start = text.find(&format!("const {key} = \"")).unwrap() + key.len() + 10;
                            text[start..start + text[start..].find('"').unwrap()].to_owned()
                        };
                        let (service, token) = (value("SERVICE"), value("TOKEN"));
                        let windows = r#"[{"id":"a","title":"Game","app":"game","pid":1,"sizes":[[1280,720]]},{"id":"b","title":"Editor","app":"kate","pid":2}]"#;
                        fake.call_method(Some(service.as_str()), PATH, Some(IFACE), "Windows", &(token.as_str(), windows))
                            .await
                            .unwrap();
                        fake.call_method(Some(service.as_str()), PATH, Some(IFACE), "Front", &(token.as_str(), r#"{"active":"a","pointer":true}"#))
                            .await
                            .unwrap();
                        // Someone else on the bus, with the token even.
                        let spoof = zbus::Connection::session().await.unwrap();
                        let refused = spoof
                            .call_method(Some(service.as_str()), PATH, Some(IFACE), "Front", &(token.as_str(), r#"{"active":"b","pointer":true}"#))
                            .await;
                        assert!(refused.is_err(), "a push from another program is refused");
                    }
                    Some("unloadScript") => {
                        unloaded_flag.fetch_add(1, Ordering::SeqCst);
                        fake.send(&reply.build(&(true,)).unwrap()).await.unwrap();
                    }
                    _ => {}
                }
            }
        });
        let dir = std::env::temp_dir().join(format!("couch-front-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let watch = watch(&dir, "test").unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(5);
        let game = loop {
            let windows = windows();
            if windows.len() == 2 && told().active != 0 {
                break windows[0].id;
            }
            assert!(
                std::time::Instant::now() < until,
                "the stand-in's pushes never came"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        // The spoofed push (the editor in front) came after: still the game.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(focus(game), (true, true));
        assert!(dir.join("front-kwin.js").exists());
        // KWin leaves the bus (it crashed): nothing in front at once, long
        // before the pushes go stale.
        let old_game = game;
        rt.block_on(kwin.release_name(KWIN)).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(1);
        while focus(game) != (false, false) {
            assert!(
                std::time::Instant::now() < until,
                "a KWin gone still counted"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // It comes back: the script is loaded again and speaks. Ids from
        // another shell name nothing, so the game has a new one (the host
        // is asked again), never the old.
        rt.block_on(kwin.request_name(KWIN)).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(8);
        let game = loop {
            if let Some(w) = windows().into_iter().find(|w| w.title == "Game")
                && focus(w.id) == (true, true)
            {
                break w.id;
            }
            assert!(
                std::time::Instant::now() < until,
                "the script was never loaded again"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_ne!(game, old_game, "an id from the last shell is never reused");
        assert_eq!(focus(old_game), (false, false));
        drop(watch);
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while unloaded.load(Ordering::SeqCst) < 2
            || dir.join("front-kwin.js").exists()
            || focus(game) != (false, false)
        {
            assert!(
                std::time::Instant::now() < until,
                "the script was never unloaded"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            focus(game),
            (false, false),
            "nothing in front once the watch ends"
        );
        drop(kwin);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_names_keep_to_safe_characters() {
        assert_eq!(plugin_part("alice"), "alice");
        assert_eq!(plugin_part("a b/../c"), "abc");
        assert_eq!(plugin_part(""), "default");
    }
}
