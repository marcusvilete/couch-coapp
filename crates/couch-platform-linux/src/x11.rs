//! A Linux host on an X11 desktop (M28, ADR 0015): the X server itself
//! lists the windows and screens, captures them ([`capture`]), and says
//! which window has the input focus and where the pointer is
//! ([`tracker`]). The screen-sharing portal refuses X11 (xdg-desktop-portal-kde:
//! "Portal request didn't succeed: Other", a Steam Deck's Desktop Mode), so
//! this path stands in for it there, and only there.
//!
//! - **Only on an X11 session** ([`session`]): on Wayland, `DISPLAY` is
//!   XWayland's, whose root window holds only X clients, so a capture of it
//!   would stream a black screen with a few windows on it.
//! - **Through x11rb,** the X protocol in Rust (no libxcb), each user on a
//!   connection of its own: a listing opens one and closes it, a capture
//!   keeps its own, the tracker its own.
//! - **Nothing here acts on a window:** it reads, and captures.

use std::sync::OnceLock;

use x11rb::{
    connection::Connection,
    protocol::{
        randr::ConnectionExt as _,
        xproto::{self, AtomEnum, ConnectionExt as _},
    },
    rust_connection::RustConnection,
};

pub mod capture;
pub mod keys;
pub mod tracker;

/// Windows listed at most, and the longest title or program name kept.
const MAX_WINDOWS: u32 = 1024;
const MAX_TEXT: usize = 512;
/// The longest window title read, in 32-bit units (4 KiB).
const MAX_TITLE_UNITS: u32 = 1024;

/// Whether this desktop session is X11, decided once from the environment
/// (a session doesn't change type while the app runs), and logged.
pub fn session() -> bool {
    static X11: OnceLock<bool> = OnceLock::new();
    *X11.get_or_init(|| {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let session_type = var("XDG_SESSION_TYPE");
        let x11 = decide(
            session_type.as_deref(),
            var("WAYLAND_DISPLAY").is_some(),
            var("DISPLAY").as_deref().is_some_and(local),
        );
        tracing::info!(
            x11,
            session_type = session_type.as_deref().unwrap_or("unset"),
            "the desktop session"
        );
        x11
    })
}

/// X11 only with an X display and no Wayland one: `XDG_SESSION_TYPE`
/// saying `wayland`, or a Wayland display set, is never X11, whatever
/// `DISPLAY` says (XWayland's).
fn decide(session_type: Option<&str>, wayland_display: bool, display: bool) -> bool {
    if !display || wayland_display {
        return false;
    }
    !session_type.is_some_and(|t| t.trim().eq_ignore_ascii_case("wayland"))
}

/// Whether `display` is this PC's own X server (`:0`, `unix:0`), never one
/// reached over the network (`ssh -X`'s `localhost:10`): the `uinput`
/// keyboard and mouse reach this PC's, so what is in front must be read
/// from it (M28's review).
fn local(display: &str) -> bool {
    let display = display.trim();
    display.starts_with(':') || display.starts_with("unix:")
}

/// Whose a window is: its process (`_NET_WM_PID`) and window class, to tell
/// a game's window by its id again (a game that reopened its X connection)
/// from another program's that got the same id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) pid: Option<u32>,
    pub(crate) class: Option<String>,
}

impl Identity {
    /// Whether `other` is the same program's: by process where both say it,
    /// else by window class; `None` while it can't be told (a new window's
    /// properties come a moment after it).
    pub(crate) fn same(&self, other: &Identity) -> Option<bool> {
        match (self.pid, other.pid) {
            (Some(a), Some(b)) => Some(a == b),
            _ => match (&self.class, &other.class) {
                (Some(a), Some(b)) => Some(a == b),
                _ => None,
            },
        }
    }

    /// Whether anything is known of it.
    pub(crate) fn known(&self) -> bool {
        self.pid.is_some() || self.class.is_some()
    }

    /// `window`'s, read from the server; `pid_atom` is `_NET_WM_PID`.
    pub(crate) fn of(
        conn: &RustConnection,
        pid_atom: u32,
        window: u32,
    ) -> Result<Self, x11rb::errors::ConnectionError> {
        let read = |property: u32, kind: AtomEnum, units: u32| {
            tracker::reply(conn.get_property(false, window, property, kind, 0, units))
        };
        let pid = read(pid_atom, AtomEnum::CARDINAL, 1)?
            .and_then(|p| p.value32().and_then(|mut v| v.next()));
        let class = read(AtomEnum::WM_CLASS.into(), AtomEnum::STRING, 256)?
            .map(|p| class_name(&p.value))
            .filter(|c| !c.is_empty());
        Ok(Self { pid, class })
    }
}

/// `_NET_WM_PID`'s atom.
pub(crate) fn pid_atom(conn: &RustConnection) -> Result<u32, x11rb::errors::ReplyError> {
    Ok(conn.intern_atom(false, b"_NET_WM_PID")?.reply()?.atom)
}

/// A window the host could stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// The client's window (not the window manager's frame around it).
    pub id: u32,
    pub title: String,
    /// The program, by its window class (`WM_CLASS`); may be empty.
    pub app: String,
    /// Its process, as the window says (`_NET_WM_PID`); may be wrong or
    /// missing.
    pub pid: Option<u32>,
}

/// A screen the host could stream: a RandR monitor, a rectangle of the
/// root window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The monitor's name as an atom (stable while the server runs), or the
    /// root window's id for the whole root where RandR lists none.
    pub id: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// A connection to the X server `DISPLAY` names, and its default screen's
/// root window.
pub(crate) fn connect() -> Result<(RustConnection, xproto::Window), String> {
    let (conn, screen) =
        x11rb::connect(None).map_err(|e| format!("the X server can't be reached: {e}"))?;
    let root = conn
        .setup()
        .roots
        .get(screen)
        .map(|s| s.root)
        .ok_or("the X server has no such screen")?;
    Ok((conn, root))
}

/// The atoms the lists read.
pub(crate) struct Atoms {
    pub client_list: u32,
    pub wm_name: u32,
    pub utf8: u32,
    pub pid: u32,
    pub window_type: u32,
    pub type_dock: u32,
    pub type_desktop: u32,
}

impl Atoms {
    pub(crate) fn new(conn: &RustConnection) -> Result<Self, String> {
        let names: [&[u8]; 7] = [
            b"_NET_CLIENT_LIST",
            b"_NET_WM_NAME",
            b"UTF8_STRING",
            b"_NET_WM_PID",
            b"_NET_WM_WINDOW_TYPE",
            b"_NET_WM_WINDOW_TYPE_DOCK",
            b"_NET_WM_WINDOW_TYPE_DESKTOP",
        ];
        let cookies = names
            .iter()
            .map(|n| conn.intern_atom(false, n))
            .collect::<Result<Vec<_>, _>>()
            .map_err(failed)?;
        let mut atoms = Vec::with_capacity(cookies.len());
        for cookie in cookies {
            atoms.push(cookie.reply().map_err(failed)?.atom);
        }
        Ok(Self {
            client_list: atoms[0],
            wm_name: atoms[1],
            utf8: atoms[2],
            pid: atoms[3],
            window_type: atoms[4],
            type_dock: atoms[5],
            type_desktop: atoms[6],
        })
    }
}

/// The windows the window manager lists (`_NET_CLIENT_LIST`), this process's
/// and the desktop's own (docks, the desktop) left out, and those without a
/// title. Blocks briefly.
pub fn windows() -> Result<Vec<Window>, String> {
    let (conn, root) = connect()?;
    let atoms = Atoms::new(&conn)?;
    let ids: Vec<u32> = conn
        .get_property(
            false,
            root,
            atoms.client_list,
            AtomEnum::WINDOW,
            0,
            MAX_WINDOWS,
        )
        .map_err(failed)?
        .reply()
        .map_err(failed)?
        .value32()
        .map(Iterator::collect)
        .unwrap_or_default();
    if ids.is_empty() {
        tracing::info!("the window manager lists no windows (_NET_CLIENT_LIST)");
    }
    let ours = std::process::id();
    // Every window's properties asked at once, then read: one wait, not
    // four per window.
    let asked: Vec<_> = ids
        .iter()
        .map(|&id| {
            let get = |property: u32, kind: u32, units: u32| {
                conn.get_property(false, id, property, kind, 0, units).ok()
            };
            (
                id,
                get(atoms.wm_name, atoms.utf8, MAX_TITLE_UNITS),
                get(
                    AtomEnum::WM_NAME.into(),
                    AtomEnum::ANY.into(),
                    MAX_TITLE_UNITS,
                ),
                get(AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into(), 256),
                get(atoms.pid, AtomEnum::CARDINAL.into(), 1),
                get(atoms.window_type, AtomEnum::ATOM.into(), 16),
            )
        })
        .collect();
    let mut windows = Vec::new();
    for (id, net_name, name, class, pid, kind) in asked {
        let read = |c: Option<
            x11rb::cookie::Cookie<'_, RustConnection, xproto::GetPropertyReply>,
        >| { c.and_then(|c| c.reply().ok()) };
        let (net_name, name, class, pid, kind) = (
            read(net_name),
            read(name),
            read(class),
            read(pid),
            read(kind),
        );
        let kinds: Vec<u32> = kind
            .as_ref()
            .and_then(|k| k.value32())
            .map(Iterator::collect)
            .unwrap_or_default();
        if kinds.contains(&atoms.type_dock) || kinds.contains(&atoms.type_desktop) {
            continue;
        }
        let pid = pid.and_then(|p| p.value32().and_then(|mut v| v.next()));
        // This app's, and its stream windows' (the host's own among them,
        // which would capture itself).
        if pid.is_some_and(|pid| pid == ours || stream_window_of_ours(pid, ours)) {
            continue;
        }
        let title = net_name
            .map(|p| p.value)
            .filter(|v| !v.is_empty())
            .or_else(|| name.map(|p| p.value))
            .map(|v| text(&v))
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        let app = class.map(|c| class_name(&c.value)).unwrap_or_default();
        windows.push(Window {
            id,
            title,
            app,
            pid,
        });
    }
    Ok(windows)
}

/// Whether `pid` is one of this app's stream windows (`couch-viewer`, by
/// its process name, a descendant of `ours`).
fn stream_window_of_ours(pid: u32, ours: u32) -> bool {
    crate::sound::is_ours(pid, ours)
        && std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .is_ok_and(|name| name.trim() == "couch-viewer")
}

/// The screens RandR lists (its monitors), or the whole root where it lists
/// none. Blocks briefly.
pub fn screens() -> Result<Vec<Screen>, String> {
    let (conn, root) = connect()?;
    screens_on(&conn, root)
}

pub(crate) fn screens_on(
    conn: &RustConnection,
    root: xproto::Window,
) -> Result<Vec<Screen>, String> {
    let monitors = conn
        .randr_query_version(1, 5)
        .ok()
        .and_then(|c| c.reply().ok())
        .filter(|v| (v.major_version, v.minor_version) >= (1, 5))
        .and_then(|_| conn.randr_get_monitors(root, true).ok())
        .and_then(|c| c.reply().ok())
        .map(|r| r.monitors)
        .unwrap_or_default();
    let mut screens = Vec::new();
    for m in monitors {
        if m.width == 0 || m.height == 0 {
            continue;
        }
        let name = conn
            .get_atom_name(m.name)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| text(&r.name))
            .unwrap_or_default();
        screens.push(Screen {
            id: m.name,
            name,
            x: m.x.into(),
            y: m.y.into(),
            width: m.width.into(),
            height: m.height.into(),
            primary: m.primary,
        });
    }
    if screens.is_empty() {
        let geometry = conn
            .get_geometry(root)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        screens.push(Screen {
            id: root,
            name: "The whole screen".into(),
            x: 0,
            y: 0,
            width: geometry.width.into(),
            height: geometry.height.into(),
            primary: true,
        });
    } else if !screens.iter().any(|s| s.primary) {
        screens[0].primary = true;
    }
    Ok(screens)
}

/// The process `window` says it belongs to (`_NET_WM_PID`), for following a
/// game to its next window. Blocks briefly.
pub fn pid_of(window: u32) -> Option<u32> {
    let (conn, _) = connect().ok()?;
    let atom = conn
        .intern_atom(false, b"_NET_WM_PID")
        .ok()?
        .reply()
        .ok()?
        .atom;
    conn.get_property(false, window, atom, AtomEnum::CARDINAL, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()
}

/// Asks the window manager to bring `window` to the front (M25b's Bring to
/// front on an X11 desktop): EWMH's `_NET_ACTIVE_WINDOW`, sent as a taskbar
/// sends it (source 2, which window managers honour where they'd refuse an
/// application), restoring the window if it is minimized. Only ever the
/// window streamed (the engine's rule). Whether it came is the window
/// manager's: the tracker sees it. Blocks briefly.
pub fn activate(window: u32) -> Result<(), String> {
    let (conn, root) = connect()?;
    let atom = conn
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")
        .map_err(failed)?
        .reply()
        .map_err(failed)?
        .atom;
    let message = xproto::ClientMessageEvent::new(32, window, atom, [2u32, 0, 0, 0, 0]);
    conn.send_event(
        false,
        root,
        xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
        message,
    )
    .map_err(failed)?
    .check()
    .map_err(failed)?;
    Ok(())
}

/// A property's text, as UTF-8 where it is, cut to [`MAX_TEXT`] bytes at a
/// character, control characters dropped.
fn text(bytes: &[u8]) -> String {
    let text: String = String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let mut end = text.len().min(MAX_TEXT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].trim().to_string()
}

/// The class in `WM_CLASS` (`instance\0class\0`), else its instance.
fn class_name(bytes: &[u8]) -> String {
    let mut parts = bytes.split(|&b| b == 0).filter(|p| !p.is_empty());
    let instance = parts.next();
    parts.next().or(instance).map(text).unwrap_or_default()
}

pub(crate) fn failed(e: impl std::fmt::Display) -> String {
    format!("the X server: {e}")
}

/// What the X11 tests share: a window on the X server `DISPLAY` names (in
/// WSL, Xvfb with openbox: the `linux-checks` skill).
#[cfg(test)]
pub(crate) mod testing {
    use std::time::{Duration, Instant};

    use x11rb::{
        connection::Connection as _,
        protocol::xproto::{ConnectionExt as _, InputFocus},
    };

    /// A test window, closed when dropped.
    pub(crate) struct TestWindow(std::process::Child);

    impl TestWindow {
        /// `xclock` (x11-apps) at 300 by 200, titled `title`, once the
        /// window manager lists it; and its id.
        pub(crate) fn open(title: &str) -> (Self, u32) {
            Self::open_with(title, &[])
        }

        /// The same with more of `xclock`'s arguments (`-bg red`).
        pub(crate) fn open_with(title: &str, more: &[&str]) -> (Self, u32) {
            let child = std::process::Command::new("xclock")
                .args([
                    "-title",
                    title,
                    "-geometry",
                    "300x200+40+50",
                    "-update",
                    "1",
                ])
                .args(more)
                .spawn()
                .expect("xclock (x11-apps) runs");
            let window = Self(child);
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(w) = super::windows()
                    .expect("windows listed")
                    .into_iter()
                    .find(|w| w.title == title)
                {
                    return (window, w.id);
                }
                assert!(
                    Instant::now() < deadline,
                    "the window manager never listed {title}"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Gives `window` the input focus, as a window manager does.
    pub(crate) fn focus(window: u32) {
        let (conn, _) = super::connect().expect("the X server");
        conn.set_input_focus(InputFocus::PARENT, window, 0u32)
            .expect("sent")
            .check()
            .expect("focused");
        conn.flush().expect("flushed");
    }

    /// Waits up to a few seconds for `ok`.
    pub(crate) fn until(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_only_without_wayland() {
        assert!(decide(Some("x11"), false, true));
        // A session started some other way (a tty, `startx`) with only an
        // X display.
        assert!(decide(None, false, true));
        assert!(decide(Some("tty"), false, true));
        // Wayland, whatever DISPLAY says: XWayland's.
        assert!(!decide(Some("wayland"), true, true));
        assert!(!decide(Some("Wayland"), false, true));
        assert!(!decide(Some("x11"), true, true));
        assert!(!decide(None, true, true));
        // No display at all.
        assert!(!decide(Some("x11"), false, false));
    }

    #[test]
    fn only_this_pcs_own_x_server() {
        assert!(local(":0"));
        assert!(local(":1.0"));
        assert!(local("unix:0"));
        // Forwarded over SSH, or any other host's: never ours to read.
        assert!(!local("localhost:10.0"));
        assert!(!local("192.168.1.5:0"));
        assert!(!local(""));
    }

    #[test]
    fn a_window_is_the_same_programs_by_process_else_by_class() {
        let game = Identity {
            pid: Some(10),
            class: Some("supertuxkart".into()),
        };
        let reopened = Identity {
            pid: Some(10),
            class: None,
        };
        let other = Identity {
            pid: Some(11),
            class: Some("supertuxkart".into()),
        };
        assert_eq!(game.same(&reopened), Some(true));
        assert_eq!(game.same(&other), Some(false), "the process decides first");
        let by_class = Identity {
            pid: None,
            class: Some("xterm".into()),
        };
        assert_eq!(game.same(&by_class), Some(false));
        assert_eq!(game.same(&Identity::default()), None, "not told yet");
    }

    #[test]
    fn names_from_properties() {
        assert_eq!(
            class_name(b"steam_app_1234\0steam_app_1234\0"),
            "steam_app_1234"
        );
        assert_eq!(class_name(b"xterm\0XTerm\0"), "XTerm");
        assert_eq!(class_name(b"only\0"), "only");
        assert_eq!(class_name(b""), "");
        assert_eq!(text(b"  A \x07game\n "), "A game");
        let long = "é".repeat(400);
        let cut = text(long.as_bytes());
        assert!(cut.len() <= MAX_TEXT && cut.chars().all(|c| c == 'é'));
    }
}
