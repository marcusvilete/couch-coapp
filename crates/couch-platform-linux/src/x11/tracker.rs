//! What the X server says of the streamed window (M28, ADR 0015): whether
//! it is still there, whether it is in front, whether the pointer is over
//! it, where its client area is, and where the pointer is.
//!
//! - **Read by a thread of its own,** on a connection of its own, every
//!   [`POLL`], while anything asked in the last [`IDLE`]: the input task
//!   asks every few milliseconds and must never wait on the X server, so
//!   [`focus`], [`pointer`] and [`area`] read the last look, never the
//!   server.
//! - **A look counts for [`STALE`]:** an X server that stops answering
//!   leaves nothing in front and no pointer to keep in place.
//! - **In front** (M28-2) means the input focus is the window or one of its
//!   children (`GetInputFocus`), **no other client holds a keyboard grab,**
//!   and the lock screen isn't up. A grab leaves the focus where it was, so
//!   the window's own focus events tell it: the server sends the focused
//!   window `FocusOut` with mode Grab as another client grabs the keyboard
//!   (a lock screen, KRunner, an overview, a menu, a run dialog), and
//!   `FocusIn` with mode Ungrab as the grab ends. A grab of the window's own
//!   (a game grabbing its keyboard on itself or a child) is the game's. An
//!   event another client sent (`SendEvent`, its top bit set) counts for
//!   nothing. A grab already held when the window is first watched isn't
//!   seen: the watch starts with the stream, at the host's click in the app.
//! - **The pointer over it** means the deepest window under the pointer is
//!   the window or one of its children (`QueryPointer` from the root down:
//!   not its frame's title bar, nor another window over it), with no grab
//!   elsewhere.

use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, MutexGuard, OnceLock, PoisonError},
    time::{Duration, Instant},
};

use super::Identity;
use x11rb::{
    connection::Connection as _,
    cookie::Cookie,
    errors::{ConnectionError, ReplyError},
    protocol::{
        Event,
        xproto::{
            self, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, MapState, NotifyDetail,
            NotifyMode,
        },
    },
    rust_connection::RustConnection,
};

/// How often the thread looks.
const POLL: Duration = Duration::from_millis(16);
/// How long a look counts.
pub const STALE: Duration = Duration::from_secs(1);
/// The thread stops once nothing asked for this long.
const IDLE: Duration = Duration::from_secs(5);
/// How often the lock screen is asked about (logind, over the bus).
const LOCK_EVERY: Duration = Duration::from_millis(500);
/// Windows watched at once; the one asked longest ago goes first.
const MAX_WATCHED: usize = 16;
/// The deepest window tree followed.
const MAX_DEPTH: usize = 32;
/// `GetInputFocus`'s focus values that aren't windows.
const FOCUS_NONE: u32 = 0;
const FOCUS_POINTER_ROOT: u32 = 1;
/// An event's code with this bit set came from `SendEvent`.
const SENT: u8 = 0x80;

/// Where the pointer is, as the X server last said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pointer {
    /// On the root window, in pixels.
    pub x: i32,
    pub y: i32,
    /// Counts the position's changes: a new count is a new position.
    pub changes: u64,
    /// When this position was first read.
    pub came: Instant,
    /// When it was last read, the same or not.
    pub told: Instant,
}

/// A window's client area on the root window, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Default)]
struct State {
    running: bool,
    /// When anything last asked.
    asked: Option<Instant>,
    windows: HashMap<u32, Seen>,
    pointer: Option<Pointer>,
}

struct Seen {
    asked: Instant,
    look: Option<Look>,
}

#[derive(Debug, Clone, Copy)]
struct Look {
    at: Instant,
    alive: bool,
    in_front: bool,
    pointer_inside: bool,
    area: Option<Area>,
}

fn lock() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The window's last look, if it counts still, asking for the next.
fn fresh_look(window: u32) -> Option<Look> {
    let now = Instant::now();
    let mut state = lock();
    ask(&mut state, Some(window), now);
    state
        .windows
        .get(&window)
        .and_then(|s| s.look)
        .filter(|l| now.saturating_duration_since(l.at) < STALE && l.alive)
}

/// Whether `window` is in front (it or a child has the input focus, no
/// other client's keyboard grab, no lock screen), and whether the pointer is
/// over it, as last looked; neither while the server hasn't said lately.
/// Returns at once.
pub fn focus(window: u32) -> (bool, bool) {
    fresh_look(window).map_or((false, false), |l| (l.in_front, l.pointer_inside))
}

/// `window`'s client area on the root, as last looked. Returns at once.
pub fn area(window: u32) -> Option<Area> {
    fresh_look(window).and_then(|l| l.area)
}

/// Whether `window` is still there, as last looked. Returns at once (the
/// pipeline asks with its locks held, M28's review): a window not looked
/// at lately counts as there, and the capture notices one that went by
/// itself.
pub fn exists(window: u32) -> bool {
    let asked = Instant::now();
    let mut state = lock();
    ask(&mut state, Some(window), asked);
    state
        .windows
        .get(&window)
        .and_then(|s| s.look)
        .filter(|l| asked.saturating_duration_since(l.at) < STALE)
        .is_none_or(|l| l.alive)
}

/// Where the pointer is, as last looked; `None` while the server hasn't
/// said lately. Returns at once.
pub fn pointer() -> Option<Pointer> {
    let now = Instant::now();
    let mut state = lock();
    ask(&mut state, None, now);
    state
        .pointer
        .filter(|p| now.saturating_duration_since(p.told) < STALE)
}

/// Notes what is asked, and starts the thread if it isn't running.
fn ask(state: &mut State, window: Option<u32>, now: Instant) {
    state.asked = Some(now);
    if let Some(window) = window {
        match state.windows.get_mut(&window) {
            Some(seen) => seen.asked = now,
            None => {
                if state.windows.len() >= MAX_WATCHED
                    && let Some(&oldest) = state
                        .windows
                        .iter()
                        .min_by_key(|(_, s)| s.asked)
                        .map(|(id, _)| id)
                {
                    state.windows.remove(&oldest);
                }
                state.windows.insert(
                    window,
                    Seen {
                        asked: now,
                        look: None,
                    },
                );
            }
        }
    }
    if !state.running {
        match std::thread::Builder::new()
            .name("x11-tracker".into())
            .spawn(run)
        {
            Ok(_) => state.running = true,
            Err(e) => tracing::warn!("the X server can't be watched: {e}"),
        }
    }
}

/// What one connection knows of its watched windows: whose events it
/// receives, which have seen another client grab the keyboard or the
/// pointer, and whose each window was when first watched. All start over
/// with a new connection.
#[derive(Default)]
struct Watching {
    selected: HashSet<u32>,
    grabbed: HashSet<u32>,
    pointer_grabbed: HashSet<u32>,
    /// Whose each window was when first selected: a window by its id again
    /// (after it was destroyed) counts only if it is the same program's.
    whose: HashMap<u32, Identity>,
    /// Windows by a watched id found to be another program's (logged once).
    foreign: HashSet<u32>,
}

impl Watching {
    /// Takes one event: a window's focus leaving for another client's grab,
    /// or coming back; the pointer's the same; the window destroyed.
    fn event(&mut self, event: &Event) {
        // The window destroyed: whatever bears its id next isn't watched
        // until it proves the same program's.
        if let Event::DestroyNotify(e) = event
            && e.response_type & SENT == 0
        {
            self.selected.remove(&e.window);
            self.grabbed.remove(&e.window);
            self.pointer_grabbed.remove(&e.window);
            return;
        }
        match event {
            // Another client's keyboard grab, as it starts, or the focus
            // arriving while one is held (a switcher under its own grab:
            // the keys still go to the grab's holder). Never the game's own
            // grab on one of its children.
            Event::FocusOut(e) | Event::FocusIn(e)
                if e.response_type & SENT == 0
                    && e.detail != NotifyDetail::INFERIOR
                    && (e.mode == NotifyMode::GRAB
                        || (e.mode == NotifyMode::WHILE_GRABBED
                            && matches!(event, Event::FocusIn(_)))) =>
            {
                self.grabbed.insert(e.event);
            }
            // No grab any more: its end, or the focus arriving normally.
            Event::FocusIn(e)
                if e.response_type & SENT == 0
                    && (e.mode == NotifyMode::UNGRAB || e.mode == NotifyMode::NORMAL) =>
            {
                self.grabbed.remove(&e.event);
            }
            // Another client grabbed the pointer while it was over the
            // window (a window manager's move, a picker): clicks go there.
            Event::LeaveNotify(e)
                if e.response_type & SENT == 0
                    && e.mode == NotifyMode::GRAB
                    && e.detail != NotifyDetail::INFERIOR =>
            {
                self.pointer_grabbed.insert(e.event);
            }
            Event::EnterNotify(e)
                if e.response_type & SENT == 0
                    && (e.mode == NotifyMode::UNGRAB || e.mode == NotifyMode::NORMAL) =>
            {
                self.pointer_grabbed.remove(&e.event);
            }
            _ => {}
        }
    }

    /// Whether `window` may be watched now: first seen, or the same
    /// program's as when first seen (`identity`, read now).
    fn admits(&mut self, window: u32, identity: Identity) -> bool {
        match self.whose.get(&window) {
            None => {
                self.whose.insert(window, identity);
                true
            }
            // Nothing was known of the first: nothing to hold it to.
            Some(first) if !first.known() => true,
            Some(first) => match first.same(&identity) {
                Some(true) => true,
                Some(false) => {
                    if self.foreign.insert(window) {
                        tracing::info!(
                            "another program's window has the game window's id: never in front"
                        );
                    }
                    false
                }
                // Not said yet whose it is: next look.
                None => false,
            },
        }
    }
}

fn run() {
    let mut conn: Option<(RustConnection, xproto::Window, u32)> = None;
    let mut watching = Watching::default();
    let mut warned = false;
    let mut locked = (false, None::<Instant>);
    loop {
        let wanted: Vec<u32> = {
            let mut state = lock();
            let now = Instant::now();
            if state
                .asked
                .is_none_or(|a| now.saturating_duration_since(a) > IDLE)
            {
                state.running = false;
                state.pointer = None;
                return;
            }
            state
                .windows
                .retain(|_, s| now.saturating_duration_since(s.asked) <= IDLE);
            state.windows.keys().copied().collect()
        };
        if conn.is_none() {
            let connected = super::connect().and_then(|(c, root)| {
                let atom = super::pid_atom(&c).map_err(super::failed)?;
                Ok((c, root, atom))
            });
            match connected {
                Ok(c) => {
                    conn = Some(c);
                    watching = Watching::default();
                    warned = false;
                }
                Err(e) => {
                    if !warned {
                        tracing::warn!("what is in front can't be known: {e}");
                        warned = true;
                    }
                    std::thread::sleep(STALE);
                    continue;
                }
            }
        }
        let Some((c, root, pid_atom)) = conn.as_ref() else {
            continue;
        };
        if locked.1.is_none_or(|at| at.elapsed() >= LOCK_EVERY) {
            locked = (crate::session_lock::locked(), Some(Instant::now()));
        }
        let began = Instant::now();
        match look(c, *root, *pid_atom, &wanted, &mut watching, locked.0) {
            Ok((at, looks)) => {
                let mut state = lock();
                state.pointer = Some(match state.pointer {
                    Some(p) if (p.x, p.y) == at => Pointer { told: began, ..p },
                    last => Pointer {
                        x: at.0,
                        y: at.1,
                        changes: last.map_or(0, |p| p.changes.wrapping_add(1)),
                        came: began,
                        told: began,
                    },
                });
                for (window, look) in looks {
                    if let Some(seen) = state.windows.get_mut(&window) {
                        seen.look = Some(look);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("the X server stopped answering: {e}");
                conn = None;
                // Nothing in front and no pointer from now on, not for
                // another second (M28's review).
                let mut state = lock();
                state.pointer = None;
                for seen in state.windows.values_mut() {
                    seen.look = None;
                }
            }
        }
        std::thread::sleep(POLL);
    }
}

/// What one look found: the pointer's position, and each wanted window's
/// look.
type Found = ((i32, i32), Vec<(u32, Look)>);

/// Looks at the pointer and the wanted windows, after the focus events
/// that came since the last look. Fails only when the connection does; a
/// window gone meanwhile is a look saying so.
fn look(
    conn: &RustConnection,
    root: xproto::Window,
    pid_atom: u32,
    wanted: &[u32],
    watching: &mut Watching,
    locked: bool,
) -> Result<Found, ConnectionError> {
    // Events on windows newly watched, before anything is read of them: a
    // grab from now on is seen. A window by an id watched before (it was
    // destroyed) only if it is the same program's.
    for &window in wanted {
        if !watching.selected.contains(&window) {
            let identity = Identity::of(conn, pid_atom, window)?;
            if !watching.admits(window, identity) {
                continue;
            }
            // Its focus and the pointer crossing it (grabs elsewhere), and
            // its end (`DestroyNotify`): a window by its id later is
            // another, to be selected anew.
            let mask = ChangeWindowAttributesAux::new().event_mask(
                EventMask::FOCUS_CHANGE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::ENTER_WINDOW
                    | EventMask::LEAVE_WINDOW,
            );
            match conn.change_window_attributes(window, &mask)?.check() {
                Ok(()) => {
                    watching.selected.insert(window);
                }
                Err(ReplyError::X11Error(_)) => {}
                Err(ReplyError::ConnectionError(e)) => return Err(e),
            }
        }
    }
    watching.selected.retain(|w| wanted.contains(w));
    watching.grabbed.retain(|w| wanted.contains(w));
    watching.pointer_grabbed.retain(|w| wanted.contains(w));
    while let Some(event) = conn.poll_for_event()? {
        watching.event(&event);
    }
    let at = Instant::now();
    let focus = reply(conn.get_input_focus())?.map_or(FOCUS_NONE, |f| f.focus);
    // From the root down to the deepest window under the pointer.
    let Some(top) = reply(conn.query_pointer(root))? else {
        return Err(ConnectionError::UnknownError);
    };
    let position = (i32::from(top.root_x), i32::from(top.root_y));
    let mut under = Vec::new();
    let mut child = if top.same_screen { top.child } else { 0 };
    while child != 0 && under.len() < MAX_DEPTH {
        under.push(child);
        child = reply(conn.query_pointer(child))?.map_or(0, |p| p.child);
    }
    // The focused window and its ancestors, up to the root's child.
    let mut focused = Vec::new();
    if focus != FOCUS_NONE && focus != FOCUS_POINTER_ROOT {
        let mut window = focus;
        while focused.len() < MAX_DEPTH {
            focused.push(window);
            match reply(conn.query_tree(window))? {
                Some(t) if t.parent != 0 && t.parent != root => window = t.parent,
                _ => break,
            }
        }
    }
    let mut looks = Vec::with_capacity(wanted.len());
    for &window in wanted {
        let attributes = reply(conn.get_window_attributes(window))?;
        let alive = attributes.is_some();
        if !alive {
            // Gone: a window by its id again (a game that reopened its X
            // connection) is another window, whose events must be selected
            // anew, and whose it is asked, before it counts.
            watching.selected.remove(&window);
            watching.grabbed.remove(&window);
            watching.pointer_grabbed.remove(&window);
        }
        let viewable = attributes.is_some_and(|a| a.map_state == MapState::VIEWABLE);
        // Watched (its events selected) and no grab elsewhere.
        let free = watching.selected.contains(&window) && !watching.grabbed.contains(&window);
        let pointer_inside = viewable
            && free
            && !watching.pointer_grabbed.contains(&window)
            && under.contains(&window);
        // The window itself or one of its children has the focus; or the
        // focus follows the pointer, which is over it.
        let has_focus = if focus == FOCUS_POINTER_ROOT {
            pointer_inside
        } else {
            focused.contains(&window)
        };
        let in_front = viewable && free && has_focus && !locked;
        let area = if viewable {
            let geometry = reply(conn.get_geometry(window))?;
            let corner = reply(conn.translate_coordinates(window, root, 0, 0))?;
            geometry.zip(corner).map(|(g, c)| Area {
                x: i32::from(c.dst_x),
                y: i32::from(c.dst_y),
                width: u32::from(g.width),
                height: u32::from(g.height),
            })
        } else {
            None
        };
        looks.push((
            window,
            Look {
                at,
                alive,
                in_front,
                pointer_inside,
                area,
            },
        ));
    }
    Ok((position, looks))
}

/// A reply, `None` for the server's error (a window gone meanwhile), an
/// error only for the connection's.
pub(crate) fn reply<T>(
    cookie: Result<Cookie<'_, RustConnection, T>, ConnectionError>,
) -> Result<Option<T>, ConnectionError>
where
    T: x11rb::x11_utils::TryParse,
{
    match cookie?.reply() {
        Ok(reply) => Ok(Some(reply)),
        Err(ReplyError::X11Error(_)) => Ok(None),
        Err(ReplyError::ConnectionError(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::protocol::xproto::{FOCUS_IN_EVENT, FOCUS_OUT_EVENT, FocusInEvent};

    fn focus(code: u8, window: u32, mode: NotifyMode, detail: NotifyDetail) -> Event {
        let event = FocusInEvent {
            response_type: code,
            detail,
            sequence: 0,
            event: window,
            mode,
        };
        if code & !SENT == FOCUS_IN_EVENT {
            Event::FocusIn(event)
        } else {
            Event::FocusOut(event)
        }
    }

    #[test]
    fn another_clients_grab_takes_the_window_out_of_front_until_it_ends() {
        let mut w = Watching::default();
        let game = 7;
        // KRunner grabs the keyboard: the game's focus leaves for it.
        w.event(&focus(
            FOCUS_OUT_EVENT,
            game,
            NotifyMode::GRAB,
            NotifyDetail::NONLINEAR,
        ));
        assert!(w.grabbed.contains(&game));
        // Focus moves meanwhile: still grabbed.
        w.event(&focus(
            FOCUS_IN_EVENT,
            game,
            NotifyMode::WHILE_GRABBED,
            NotifyDetail::NONLINEAR,
        ));
        assert!(w.grabbed.contains(&game));
        // The grab ends.
        w.event(&focus(
            FOCUS_IN_EVENT,
            game,
            NotifyMode::UNGRAB,
            NotifyDetail::NONLINEAR,
        ));
        assert!(!w.grabbed.contains(&game));
    }

    #[test]
    fn the_focus_arriving_under_another_clients_grab_counts_as_grabbed() {
        let mut w = Watching::default();
        let game = 7;
        // A switcher moves the focus to the game while it holds the
        // keyboard: the keys still go to the switcher (M28's review).
        w.event(&focus(
            FOCUS_IN_EVENT,
            game,
            NotifyMode::WHILE_GRABBED,
            NotifyDetail::NONLINEAR,
        ));
        assert!(w.grabbed.contains(&game));
        w.event(&focus(
            FOCUS_IN_EVENT,
            game,
            NotifyMode::UNGRAB,
            NotifyDetail::NONLINEAR,
        ));
        assert!(!w.grabbed.contains(&game));
        // The game's own grab on a child, the focus arriving: the game's.
        w.event(&focus(
            FOCUS_IN_EVENT,
            game,
            NotifyMode::WHILE_GRABBED,
            NotifyDetail::INFERIOR,
        ));
        assert!(!w.grabbed.contains(&game));
    }

    #[test]
    fn a_pointer_grab_elsewhere_takes_the_pointer_off_the_window() {
        use x11rb::protocol::xproto::{ENTER_NOTIFY_EVENT, EnterNotifyEvent, LEAVE_NOTIFY_EVENT};
        let crossing = |code: u8, mode, detail| {
            let event = EnterNotifyEvent {
                response_type: code,
                detail,
                sequence: 0,
                time: 0,
                root: 1,
                event: 7,
                child: 0,
                root_x: 0,
                root_y: 0,
                event_x: 0,
                event_y: 0,
                state: Default::default(),
                mode,
                same_screen_focus: 0,
            };
            if code & !SENT == ENTER_NOTIFY_EVENT {
                Event::EnterNotify(event)
            } else {
                Event::LeaveNotify(event)
            }
        };
        let mut w = Watching::default();
        // A window manager's move: the pointer grabbed while over the game.
        w.event(&crossing(
            LEAVE_NOTIFY_EVENT,
            NotifyMode::GRAB,
            NotifyDetail::ANCESTOR,
        ));
        assert!(w.pointer_grabbed.contains(&7));
        // A sent end counts for nothing; the server's does.
        w.event(&crossing(
            ENTER_NOTIFY_EVENT | SENT,
            NotifyMode::UNGRAB,
            NotifyDetail::ANCESTOR,
        ));
        assert!(w.pointer_grabbed.contains(&7));
        w.event(&crossing(
            ENTER_NOTIFY_EVENT,
            NotifyMode::UNGRAB,
            NotifyDetail::ANCESTOR,
        ));
        assert!(!w.pointer_grabbed.contains(&7));
        // The game grabbing the pointer on its own child: the game's.
        w.event(&crossing(
            LEAVE_NOTIFY_EVENT,
            NotifyMode::GRAB,
            NotifyDetail::INFERIOR,
        ));
        assert!(!w.pointer_grabbed.contains(&7));
    }

    #[test]
    fn a_window_by_a_watched_id_counts_only_for_the_same_program() {
        use crate::x11::Identity;
        let game = Identity {
            pid: Some(10),
            class: Some("supertuxkart".into()),
        };
        let mut w = Watching::default();
        assert!(w.admits(7, game.clone()), "first seen");
        // Destroyed, and a window by its id again.
        assert!(w.admits(7, game.clone()), "the same game's");
        let other = Identity {
            pid: Some(11),
            class: Some("konsole".into()),
        };
        assert!(!w.admits(7, other), "another program's: never watched");
        assert!(!w.admits(7, Identity::default()), "not told yet: wait");
    }

    #[test]
    fn a_destroyed_window_is_watched_anew() {
        use x11rb::protocol::xproto::{DESTROY_NOTIFY_EVENT, DestroyNotifyEvent};
        let mut w = Watching::default();
        let game = 7;
        w.selected.insert(game);
        w.event(&focus(
            FOCUS_OUT_EVENT,
            game,
            NotifyMode::GRAB,
            NotifyDetail::NONLINEAR,
        ));
        let destroyed = |code| {
            Event::DestroyNotify(DestroyNotifyEvent {
                response_type: code,
                sequence: 0,
                event: game,
                window: game,
            })
        };
        // Another client's word changes nothing.
        w.event(&destroyed(DESTROY_NOTIFY_EVENT | SENT));
        assert!(w.selected.contains(&game));
        // The server's: the next window by that id is selected again first.
        w.event(&destroyed(DESTROY_NOTIFY_EVENT));
        assert!(!w.selected.contains(&game) && !w.grabbed.contains(&game));
    }

    #[test]
    fn the_games_own_grab_and_forged_events_change_nothing() {
        let mut w = Watching::default();
        let game = 7;
        // The game grabs on a child of its own window.
        w.event(&focus(
            FOCUS_OUT_EVENT,
            game,
            NotifyMode::GRAB,
            NotifyDetail::INFERIOR,
        ));
        assert!(!w.grabbed.contains(&game));
        // Another client sends a grab's end it never had.
        w.event(&focus(
            FOCUS_OUT_EVENT,
            game,
            NotifyMode::GRAB,
            NotifyDetail::NONLINEAR,
        ));
        w.event(&focus(
            FOCUS_IN_EVENT | SENT,
            game,
            NotifyMode::UNGRAB,
            NotifyDetail::NONLINEAR,
        ));
        assert!(w.grabbed.contains(&game), "a sent event ends no grab");
        // A sent grab counts for nothing either (it could only hold keys
        // back).
        let other = 9;
        w.event(&focus(
            FOCUS_OUT_EVENT | SENT,
            other,
            NotifyMode::GRAB,
            NotifyDetail::NONLINEAR,
        ));
        assert!(!w.grabbed.contains(&other));
        // The focus coming back by itself (no grab then) clears it.
        w.event(&focus(
            FOCUS_IN_EVENT,
            game,
            NotifyMode::NORMAL,
            NotifyDetail::NONLINEAR,
        ));
        assert!(!w.grabbed.contains(&game));
    }

    /// Needs an X server with a window manager and `xclock` (the `linux-checks`
    /// skill). Run with `--ignored x11::`.
    #[test]
    #[ignore = "needs an X server, a window manager and xclock"]
    fn a_grab_elsewhere_or_the_focus_leaving_takes_the_window_out_of_front() {
        use crate::x11::testing::{TestWindow, focus as give_focus, until};
        use x11rb::protocol::xproto::{GrabMode, GrabStatus};

        let (_window, id) = TestWindow::open("couch-x11-front");
        give_focus(id);
        until("in front", || super::focus(id).0);
        let area = area(id).expect("its client area");
        assert_eq!((area.width, area.height), (300, 200));

        // Another client grabs the keyboard (a menu, KRunner, a lock screen):
        // the focus stays, the window isn't in front.
        let (other, root) = crate::x11::connect().expect("the X server");
        let grab = other
            .grab_keyboard(false, root, 0u32, GrabMode::ASYNC, GrabMode::ASYNC)
            .expect("sent")
            .reply()
            .expect("answered");
        assert_eq!(grab.status, GrabStatus::SUCCESS);
        until("out of front during the grab", || !super::focus(id).0);
        other
            .ungrab_keyboard(0u32)
            .expect("sent")
            .check()
            .expect("ungrabbed");
        until("in front again", || super::focus(id).0);

        // The pointer over it, then off it.
        let (x, y) = (area.x + 20, area.y + 20);
        let warp = |x: i32, y: i32| {
            other
                .warp_pointer(0u32, root, 0, 0, 0, 0, x as i16, y as i16)
                .expect("sent")
                .check()
                .expect("moved");
        };
        warp(x, y);
        until("the pointer over it", || super::focus(id).1);
        until("the pointer read there", || {
            pointer().is_some_and(|p| (p.x, p.y) == (x, y))
        });
        warp(1200, 700);
        until("the pointer off it", || !super::focus(id).1);

        // The focus elsewhere.
        give_focus(root);
        until("out of front", || !super::focus(id).0);
    }
}
