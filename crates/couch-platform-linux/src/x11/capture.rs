//! Capture on an X11 desktop (M28, ADR 0015): a screen or a window, read
//! from the X server at the stream's rate, with no dialog.
//!
//! - **A screen** is its RandR rectangle of the root window.
//! - **A window is read from its own picture,** so windows over it never
//!   show (they could be the host's chat, mail, anything): the capture
//!   redirects it itself (Composite's automatic redirection, which the X
//!   server undoes when the capture drops or this process goes; the window
//!   looks and behaves as before), compositor or not, and reads its pixmap
//!   each frame, as OBS does. A compositor doesn't need running: KWin in a
//!   VM wasn't compositing, and the window's rectangle on the screen showed
//!   whatever lay over it (M28's first run). Only where the redirection is
//!   refused (another program redirected that window itself) does it fall
//!   back: a compositor's pixmap of the window's frame, else the window's
//!   rectangle on the screen.
//! - **Through shared memory** (MIT-SHM 1.2, a segment the server makes and
//!   we map), else the picture over the socket, slower, logged.
//! - **Only pictures we can read:** 32 bits a pixel, blue first in memory
//!   (depth 24 or 32 on a little-endian server); anything else fails the
//!   capture with a plain reason.
//! - **The pointer drawn in** from XFixes (its shape and where it is), with
//!   `crate::cursor`, as for the portal's metadata.
//! - **A window closing ends the capture** as its source closing; one
//!   minimized or hidden sends nothing until it shows again.

use std::{
    os::fd::OwnedFd,
    ptr::NonNull,
    time::{Duration, Instant},
};

use couch_media::{BgraFrame, CaptureEvent, Capturer, MediaError, VideoFrame};
use x11rb::{
    connection::{Connection as _, RequestConnection as _},
    errors::{ConnectionError, ReplyError},
    protocol::{
        ErrorKind,
        composite::{self, ConnectionExt as _},
        shm::{self, ConnectionExt as _},
        xfixes::{self, ConnectionExt as _},
        xproto::{self, ConnectionExt as _, ImageFormat, ImageOrder, MapState},
    },
    rust_connection::RustConnection,
};

use super::{Identity, tracker::reply};
use crate::cursor::{self, Channels, Shape};

/// The largest picture read, each way.
const MAX_SIZE: u32 = 8192;
/// How often the screens' layout and the compositor are looked at again.
const RELOOK: Duration = Duration::from_secs(1);
/// How long a window gone may take to come back by the same id (the same
/// game's) before the capture ends; then the pipeline looks for the game's
/// next window (`Backend::game_process`) within its own grace.
const REAPPEAR: Duration = Duration::from_secs(2);

/// What to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A screen, by [`super::Screen::id`].
    Screen(u32),
    /// A client window, by [`super::Window::id`].
    Window(u32),
}

/// A capture from the X server, until dropped.
pub struct X11Capture {
    conn: RustConnection,
    root: xproto::Window,
    target: Target,
    interval: Duration,
    next: Instant,
    /// The compositor's selection (`_NET_WM_CM_S<n>`), and whether it had
    /// an owner when last looked.
    compositor_atom: u32,
    composited: bool,
    composite: bool,
    /// The window this capture redirected, undone when it drops.
    redirected: Option<u32>,
    /// The screen's rectangle (for a screen) and the root's size, when last
    /// looked.
    area: Option<(i32, i32, u32, u32)>,
    root_size: (u32, u32),
    relooked: Option<Instant>,
    shm: Shm,
    /// The pointer's last shape, by XFixes' serial; `None` without XFixes 2.
    cursor: Option<Option<(u32, Option<Shape>)>>,
    started: Option<Instant>,
    /// Whether the last frame came from the window's own picture, for the
    /// log when that changes.
    own_picture: Option<bool>,
    /// Why the window's own picture couldn't be read last, for the log.
    own_error: Option<String>,
    /// A redirection was refused (logged once).
    refused: bool,
    /// A picture too large to stream was met (logged once).
    too_large: bool,
    /// `_NET_WM_PID`.
    pid_atom: u32,
    /// Whose the captured window is, so one with its id again is followed
    /// only if it is the same game's.
    game: Identity,
    /// Since when the window has been gone, waiting for it to come back.
    lost: Option<Instant>,
    ended: bool,
}

/// What one look at the source gave.
enum Grab {
    Frame(BgraFrame),
    /// Nothing to show now (a window minimized or off every screen).
    Nothing,
    Gone,
}

/// Starts a capture of `target` at `fps` frames a second. Fails at once
/// when there is no such source or its pictures can't be read.
pub fn start(target: Target, fps: u32) -> Result<X11Capture, String> {
    let (conn, root) = super::connect()?;
    let setup = conn.setup();
    if setup.image_byte_order != ImageOrder::LSB_FIRST {
        return Err("this X server sends its pictures in an order we can't read".into());
    }
    let thirty_two = |depth: u8| {
        setup
            .pixmap_formats
            .iter()
            .any(|f| f.depth == depth && f.bits_per_pixel == 32 && f.scanline_pad == 32)
    };
    let screen = setup
        .roots
        .iter()
        .find(|s| s.root == root)
        .ok_or("the X server has no such screen")?;
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|d| d.visuals.iter().map(move |v| (d.depth, v)))
        .find(|(_, v)| v.visual_id == screen.root_visual);
    let readable = visual.is_some_and(|(depth, v)| {
        matches!(depth, 24 | 32)
            && thirty_two(depth)
            && (v.red_mask, v.green_mask, v.blue_mask) == (0xff_0000, 0xff00, 0xff)
    });
    if !readable {
        return Err(format!(
            "this desktop's pictures can't be streamed (depth {}): set it to 24-bit colour",
            visual.map_or(0, |(d, _)| d)
        ));
    }
    let number = setup.roots.iter().position(|s| s.root == root).unwrap_or(0);
    let compositor_atom = conn
        .intern_atom(false, format!("_NET_WM_CM_S{number}").as_bytes())
        .map_err(super::failed)?
        .reply()
        .map_err(super::failed)?
        .atom;
    let composite = has(&conn, composite::X11_EXTENSION_NAME)
        && reply(conn.composite_query_version(0, 4))
            .ok()
            .flatten()
            .is_some_and(|v| (v.major_version, v.minor_version) >= (0, 2));
    let cursor = (has(&conn, xfixes::X11_EXTENSION_NAME)
        && reply(conn.xfixes_query_version(4, 0))
            .ok()
            .flatten()
            .is_some_and(|v| v.major_version >= 2))
    .then_some(None);
    let pid_atom = conn
        .intern_atom(false, b"_NET_WM_PID")
        .map_err(super::failed)?
        .reply()
        .map_err(super::failed)?
        .atom;
    let shm = Shm::new(&conn);
    let mut capture = X11Capture {
        conn,
        root,
        target,
        interval: Duration::from_secs(1) / fps.max(1),
        next: Instant::now(),
        compositor_atom,
        composited: false,
        composite,
        redirected: None,
        area: None,
        root_size: (0, 0),
        relooked: None,
        shm,
        cursor,
        started: Some(Instant::now()),
        own_picture: None,
        own_error: None,
        refused: false,
        too_large: false,
        pid_atom,
        game: Identity::default(),
        lost: None,
        ended: false,
    };
    capture.relook().map_err(super::failed)?;
    match target {
        Target::Screen(_) if capture.area.is_none() => {
            return Err("that screen isn't there any more".into());
        }
        Target::Window(window)
            if reply(capture.conn.get_geometry(window))
                .map_err(super::failed)?
                .is_none() =>
        {
            return Err("that window isn't there any more".into());
        }
        Target::Window(window) => {
            capture.game = capture.identity(window).map_err(super::failed)?;
            capture.redirect(window).map_err(|e| e.to_string())?;
        }
        _ => {}
    }
    tracing::info!(
        ?target,
        fps,
        composite = capture.composite,
        redirected = capture.redirected.is_some(),
        compositor = capture.composited,
        shared_memory = capture.shm.usable(),
        pointer = capture.cursor.is_some(),
        "capturing from the X server"
    );
    Ok(capture)
}

fn has(conn: &RustConnection, name: &'static str) -> bool {
    conn.extension_information(name).ok().flatten().is_some()
}

impl Capturer for X11Capture {
    fn next_event(&mut self, timeout: Duration) -> Result<Option<CaptureEvent>, MediaError> {
        if self.ended {
            return Ok(Some(CaptureEvent::SourceClosed));
        }
        let now = Instant::now();
        if self.next > now {
            let wait = self.next - now;
            if wait > timeout {
                std::thread::sleep(timeout);
                return Ok(None);
            }
            std::thread::sleep(wait);
        }
        let now = Instant::now();
        self.next += self.interval;
        if self.next < now {
            // Behind (a slow read): the next frame an interval from now.
            self.next = now + self.interval;
        }
        let grab = self
            .grab()
            .map_err(|e| MediaError::Backend(e.to_string()))?;
        match grab {
            Grab::Frame(frame) => {
                if let Some(started) = self.started.take() {
                    tracing::info!(
                        after_ms = started.elapsed().as_millis() as u64,
                        width = frame.width,
                        height = frame.height,
                        "the X server's first frame"
                    );
                }
                Ok(Some(CaptureEvent::Frame(VideoFrame::Cpu(frame))))
            }
            Grab::Nothing => Ok(None),
            Grab::Gone => {
                self.ended = true;
                Ok(Some(CaptureEvent::SourceClosed))
            }
        }
    }
}

impl X11Capture {
    /// The screens' layout, the root's size, and whether a compositor runs.
    fn relook(&mut self) -> Result<(), ConnectionError> {
        let geometry = reply(self.conn.get_geometry(self.root))?;
        self.root_size = geometry.map_or((0, 0), |g| (g.width.into(), g.height.into()));
        let owner = reply(self.conn.get_selection_owner(self.compositor_atom))?;
        self.composited = owner.is_some_and(|o| o.owner != 0);
        if let Target::Screen(id) = self.target {
            self.area = super::screens_on(&self.conn, self.root)
                .ok()
                .and_then(|screens| screens.into_iter().find(|s| s.id == id))
                .map(|s| (s.x, s.y, s.width, s.height));
        }
        self.relooked = Some(Instant::now());
        Ok(())
    }

    fn grab(&mut self) -> Result<Grab, Fail> {
        if self.relooked.is_none_or(|at| at.elapsed() >= RELOOK) {
            self.relook()?;
            // A redirection refused before (a compositor's own) may be
            // possible now (compositing stopped): its own picture again,
            // never the screen for longer than it must (M28's review).
            if let Target::Window(window) = self.target
                && self.composite
                && self.redirected.is_none()
                && self.lost.is_none()
            {
                self.redirect(window)?;
            }
        }
        let (picture, at) = match self.target {
            Target::Screen(_) => {
                let Some(area) = self.area else {
                    return Ok(Grab::Gone);
                };
                match self.read(self.root, area)? {
                    Some(picture) => (picture, (area.0, area.1)),
                    None => return Ok(Grab::Nothing),
                }
            }
            Target::Window(window) => match self.read_window(window)? {
                WindowGrab::Picture(picture, at) => (picture, at),
                WindowGrab::Nothing => return Ok(Grab::Nothing),
                WindowGrab::Gone => return Ok(Grab::Gone),
            },
        };
        Ok(Grab::Frame(self.finish(picture, at)?))
    }

    /// Its own picture from now on, whatever lies over it: redirects
    /// `window` (Composite's automatic redirection), unless another program
    /// has.
    fn redirect(&mut self, window: u32) -> Result<(), Fail> {
        if !self.composite {
            return Ok(());
        }
        // Its end, on this connection: a window by its id later is another,
        // whose identity is asked before anything of it is captured.
        let mask = xproto::ChangeWindowAttributesAux::new()
            .event_mask(xproto::EventMask::STRUCTURE_NOTIFY);
        self.conn
            .change_window_attributes(window, &mask)?
            .ignore_error();
        match self
            .conn
            .composite_redirect_window(window, composite::Redirect::AUTOMATIC)?
            .check()
        {
            Ok(()) => {
                if self.redirected != Some(window) {
                    tracing::info!("the window redirected: its own picture");
                }
                self.redirected = Some(window);
            }
            // Already redirected by us (the same window, not a new one).
            Err(ReplyError::X11Error(e))
                if e.error_kind == ErrorKind::Access && self.redirected == Some(window) => {}
            Err(ReplyError::X11Error(e)) => {
                if !self.refused {
                    tracing::warn!(
                        error = ?e.error_kind,
                        "the window can't be redirected (another program has): windows over it may show"
                    );
                    self.refused = true;
                }
                self.redirected = None;
            }
            Err(ReplyError::ConnectionError(e)) => return Err(e.into()),
        }
        Ok(())
    }

    /// Whether the X server said `window` was destroyed since last asked
    /// (its `DestroyNotify`, never one another client sent): whatever bears
    /// its id now is another window. Drains this connection's events.
    fn destroyed(&mut self, window: u32) -> Result<bool, ConnectionError> {
        let mut destroyed = false;
        while let Some(event) = self.conn.poll_for_event()? {
            if let x11rb::protocol::Event::DestroyNotify(e) = event
                && e.window == window
                && e.response_type & 0x80 == 0
            {
                destroyed = true;
            }
        }
        Ok(destroyed)
    }

    /// Whose `window` is: its process (`_NET_WM_PID`) and window class.
    fn identity(&self, window: u32) -> Result<Identity, ConnectionError> {
        Identity::of(&self.conn, self.pid_atom, window)
    }

    /// The window went: a moment's wait for the game to show it again (a
    /// game switching full screen may close its X connection and open a new
    /// one, whose first window gets the same id: SuperTuxKart, M28's VM
    /// run), then gone.
    fn window_gone(&mut self, request: &str) -> WindowGrab {
        let now = Instant::now();
        match self.lost {
            None => {
                tracing::info!(
                    request,
                    "the window went: waiting a moment for it to come back"
                );
                self.lost = Some(now);
                WindowGrab::Nothing
            }
            Some(since) if now.saturating_duration_since(since) < REAPPEAR => WindowGrab::Nothing,
            Some(_) => {
                tracing::info!(request, "the window is gone");
                WindowGrab::Gone
            }
        }
    }

    /// A client window's picture and where it is on the root, never another
    /// window's that took its id: its destruction (on this connection's
    /// events, which come in order with the replies) is looked for before
    /// and after each read, and a picture read across it is dropped (M28's
    /// review: a window a compositor redirects would otherwise be read).
    fn read_window(&mut self, window: u32) -> Result<WindowGrab, Fail> {
        if self.destroyed(window)? {
            self.mark_lost();
        }
        let grab = self.read_window_now(window)?;
        if matches!(grab, WindowGrab::Picture(..)) && self.destroyed(window)? {
            self.mark_lost();
            return Ok(WindowGrab::Nothing);
        }
        Ok(grab)
    }

    /// The window was destroyed: whatever bears its id is asked whose it is
    /// before anything of it is captured.
    fn mark_lost(&mut self) {
        if self.lost.is_none() {
            tracing::info!("the window was destroyed: waiting a moment for the game's next");
            self.lost = Some(Instant::now());
        }
    }

    fn read_window_now(&mut self, window: u32) -> Result<WindowGrab, Fail> {
        let Some(geometry) = reply(self.conn.get_geometry(window))? else {
            return Ok(self.window_gone("GetGeometry"));
        };
        let Some(attributes) = reply(self.conn.get_window_attributes(window))? else {
            return Ok(self.window_gone("GetWindowAttributes"));
        };
        // A window by that id again: followed only if it is the same game's,
        // and redirected anew (the redirection went with the old one).
        if self.lost.is_some() {
            match self.game.same(&self.identity(window)?) {
                Some(true) => {
                    tracing::info!("the game's window is back");
                    self.lost = None;
                    self.redirect(window)?;
                }
                // Another program's window took the id: never captured.
                Some(false) => {
                    tracing::info!("another program's window took the game window's id");
                    return Ok(WindowGrab::Gone);
                }
                // Not said yet whose it is.
                None => return Ok(self.window_gone("an identity")),
            }
        }
        if attributes.map_state != MapState::VIEWABLE {
            return Ok(WindowGrab::Nothing);
        }
        let (width, height) = (u32::from(geometry.width), u32::from(geometry.height));
        let Some(on_root) = reply(self.conn.translate_coordinates(window, self.root, 0, 0))? else {
            return Ok(self.window_gone("TranslateCoordinates"));
        };
        let at = (i32::from(on_root.dst_x), i32::from(on_root.dst_y));
        // Redirected by us: its own pixmap, which starts at its border.
        // Never the screen then, which may show what lies over it: a frame
        // it can't give is skipped (viewers keep the last).
        if self.redirected == Some(window) {
            let border = i32::from(geometry.border_width);
            return Ok(
                match self.read_pixmap(window, (border, border, width, height))? {
                    Some(picture) => {
                        self.note_source(true);
                        WindowGrab::Picture(picture, at)
                    }
                    None => {
                        let why = self.own_error.take().unwrap_or_default();
                        // Not redirected now: maybe a new window by its id,
                        // made between two frames. The next frame asks
                        // whose it is, as for one gone and back.
                        if why.starts_with("NameWindowPixmap") && self.lost.is_none() {
                            self.lost = Some(Instant::now());
                        }
                        if self.own_picture != Some(false) {
                            self.own_picture = Some(false);
                            tracing::info!(
                                why,
                                "the window's own picture can't be read: its frames skipped until it can"
                            );
                        }
                        WindowGrab::Nothing
                    }
                },
            );
        }
        if self.composite
            && self.composited
            && let Some(picture) = self.read_own(window, width, height)?
        {
            self.note_source(true);
            return Ok(WindowGrab::Picture(picture, at));
        }
        self.note_source(false);
        // Its rectangle on the screen, cut to the root.
        let (root_w, root_h) = (self.root_size.0 as i64, self.root_size.1 as i64);
        let x0 = i64::from(at.0).clamp(0, root_w);
        let y0 = i64::from(at.1).clamp(0, root_h);
        let x1 = (i64::from(at.0) + i64::from(width)).clamp(0, root_w);
        let y1 = (i64::from(at.1) + i64::from(height)).clamp(0, root_h);
        if x1 <= x0 || y1 <= y0 {
            return Ok(WindowGrab::Nothing);
        }
        let area = (x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32);
        Ok(match self.read(self.root, area)? {
            Some(picture) => WindowGrab::Picture(picture, (area.0, area.1)),
            None => WindowGrab::Nothing,
        })
    }

    /// The window's own picture, from Composite's pixmap of its top-level
    /// window; `None` when that window isn't redirected now (a game that
    /// bypasses the compositor) or the picture can't be read.
    fn read_own(&mut self, window: u32, width: u32, height: u32) -> Result<Option<Picture>, Fail> {
        // Up to the root's child: the window manager's frame, or the window.
        let mut top = window;
        for _ in 0..32 {
            match reply(self.conn.query_tree(top))? {
                Some(t) if t.parent != 0 && t.parent != self.root => top = t.parent,
                Some(_) => break,
                None => return Ok(None),
            }
        }
        let Some(frame) = reply(self.conn.get_geometry(top))? else {
            return Ok(None);
        };
        // The client's corner in the pixmap, which starts at the frame's
        // border.
        let (x, y) = if top == window {
            (0, 0)
        } else {
            let Some(inside) = reply(self.conn.translate_coordinates(window, top, 0, 0))? else {
                return Ok(None);
            };
            (i32::from(inside.dst_x), i32::from(inside.dst_y))
        };
        let border = i32::from(frame.border_width);
        self.read_pixmap(top, (x + border, y + border, width, height))
    }

    /// `area` of a redirected `window`'s pixmap (Composite's
    /// `NameWindowPixmap`, named again each frame: a resize makes a new
    /// one); `None` when it isn't redirected now or can't be read.
    fn read_pixmap(
        &mut self,
        window: u32,
        area: (i32, i32, u32, u32),
    ) -> Result<Option<Picture>, Fail> {
        let pixmap = self
            .conn
            .generate_id()
            .map_err(|_| Fail::Conn(ConnectionError::UnknownError))?;
        match self
            .conn
            .composite_name_window_pixmap(window, pixmap)?
            .check()
        {
            Ok(()) => {}
            Err(ReplyError::X11Error(e)) => {
                self.own_error = Some(format!("NameWindowPixmap: {:?}", e.error_kind));
                return Ok(None);
            }
            Err(ReplyError::ConnectionError(e)) => return Err(e.into()),
        }
        let picture = self.read(pixmap, area);
        self.conn.free_pixmap(pixmap)?.ignore_error();
        if matches!(picture, Ok(None)) {
            self.own_error = Some(format!("GetImage of {area:?}"));
        }
        picture
    }

    /// Logs which picture a window's frames come from when that changes,
    /// and why its own couldn't be read.
    fn note_source(&mut self, own: bool) {
        let why = self.own_error.take();
        if self.own_picture != Some(own) {
            self.own_picture = Some(own);
            tracing::info!(
                own_picture = own,
                why = why.filter(|_| !own).unwrap_or_default(),
                "the window's frames: {}",
                if own {
                    "its own picture (Composite)"
                } else {
                    "its rectangle on the screen"
                }
            );
        }
    }

    /// `area` of `drawable` as packed BGRA; `None` when the server can't
    /// give it (off the drawable, gone meanwhile).
    fn read(&mut self, drawable: u32, area: (i32, i32, u32, u32)) -> Result<Option<Picture>, Fail> {
        let (x, y, width, height) = area;
        if width == 0 || height == 0 {
            return Ok(None);
        }
        if width > MAX_SIZE || height > MAX_SIZE {
            if !self.too_large {
                tracing::warn!(
                    width,
                    height,
                    "a picture larger than {MAX_SIZE} each way: not streamed"
                );
                self.too_large = true;
            }
            return Ok(None);
        }
        let (Ok(x), Ok(y)) = (i16::try_from(x), i16::try_from(y)) else {
            return Ok(None);
        };
        let (w, h) = (width as u16, height as u16);
        let bytes = width as usize * height as usize * 4;
        let captured_at = crate::clock::now();
        let picture = |data| {
            Some(Picture {
                width,
                height,
                data,
                captured_at,
            })
        };
        if let Some((seg, memory)) = self.shm.segment(&self.conn, bytes)? {
            let image = self.conn.shm_get_image(
                drawable,
                x,
                y,
                w,
                h,
                !0,
                ImageFormat::Z_PIXMAP.into(),
                seg,
                0,
            )?;
            match image.reply() {
                Ok(r) if matches!(r.depth, 24 | 32) && r.size as usize >= bytes => {
                    return Ok(picture(memory.read(bytes)));
                }
                Ok(r) => return Err(Fail::Depth(r.depth)),
                // The server won't write into the segment: over its socket
                // from now on.
                Err(ReplyError::X11Error(e)) if e.error_kind == ErrorKind::Access => {
                    tracing::warn!(
                        "the X server won't write pictures into shared memory: they come over its socket"
                    );
                    self.shm = Shm::Off;
                }
                // Off the drawable, or gone meanwhile.
                Err(ReplyError::X11Error(_)) => return Ok(None),
                Err(ReplyError::ConnectionError(e)) => return Err(e.into()),
            }
        }
        let Some(r) = reply(
            self.conn
                .get_image(ImageFormat::Z_PIXMAP, drawable, x, y, w, h, !0),
        )?
        else {
            return Ok(None);
        };
        if !matches!(r.depth, 24 | 32) || r.data.len() < bytes {
            return Err(Fail::Depth(r.depth));
        }
        let mut data = r.data;
        data.truncate(bytes);
        Ok(picture(data))
    }

    /// The frame: the picture with the pointer drawn in, if it is over it.
    fn finish(&mut self, mut picture: Picture, at: (i32, i32)) -> Result<BgraFrame, Fail> {
        if let Some(known) = &mut self.cursor
            && let Some(image) = reply(self.conn.xfixes_get_cursor_image())?
        {
            if known.as_ref().map(|(serial, _)| *serial) != Some(image.cursor_serial) {
                let shape = shape(
                    u32::from(image.width),
                    u32::from(image.height),
                    (i32::from(image.xhot), i32::from(image.yhot)),
                    &image.cursor_image,
                );
                *known = Some((image.cursor_serial, shape));
            }
            if let Some((_, Some(shape))) = known.as_ref() {
                let _ = cursor::draw(
                    &mut picture.data,
                    picture.width,
                    picture.height,
                    shape,
                    (i32::from(image.x) - at.0, i32::from(image.y) - at.1),
                );
            }
        }
        Ok(BgraFrame {
            width: picture.width,
            height: picture.height,
            stride: picture.width * 4,
            data: picture.data.into(),
            captured_at: picture.captured_at,
        })
    }
}

impl Drop for X11Capture {
    /// The window as it was. The X server does the same when this
    /// connection closes, the process killed included.
    fn drop(&mut self) {
        if let Some(window) = self.redirected.take()
            && let Ok(cookie) = self
                .conn
                .composite_unredirect_window(window, composite::Redirect::AUTOMATIC)
        {
            cookie.ignore_error();
            let _ = self.conn.flush();
        }
    }
}

/// What [`X11Capture::read_window`] gave.
enum WindowGrab {
    Picture(Picture, (i32, i32)),
    Nothing,
    Gone,
}

struct Picture {
    width: u32,
    height: u32,
    data: Vec<u8>,
    captured_at: couch_media::Micros,
}

/// Why a capture stopped.
#[derive(Debug)]
enum Fail {
    /// The connection to the X server failed.
    Conn(ConnectionError),
    /// A picture of a depth we can't read (a 30-bit window, say).
    Depth(u8),
}

impl From<ConnectionError> for Fail {
    fn from(e: ConnectionError) -> Self {
        Self::Conn(e)
    }
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conn(e) => write!(f, "the X server: {e}"),
            Self::Depth(depth) => write!(
                f,
                "a picture of {depth}-bit colour, which can't be streamed: set the desktop to 24-bit colour"
            ),
        }
    }
}

/// XFixes' pointer (premultiplied ARGB, one `u32` a pixel) as a shape with
/// straight alpha.
fn shape(width: u32, height: u32, hotspot: (i32, i32), pixels: &[u32]) -> Option<Shape> {
    let count = width as usize * height as usize;
    let pixels = pixels.get(..count)?;
    let mut bytes = Vec::with_capacity(count * 4);
    for &p in pixels {
        let [b, g, r, a] = p.to_le_bytes();
        let straight = |c: u8| {
            if a == 0 {
                0
            } else {
                (u32::from(c) * 255 / u32::from(a)).min(255) as u8
            }
        };
        bytes.extend([straight(b), straight(g), straight(r), a]);
    }
    Shape::new(
        Channels::Bgra,
        width,
        height,
        width as usize * 4,
        hotspot,
        &bytes,
    )
}

/// MIT-SHM: one segment the server makes (version 1.2), mapped here, made
/// larger as pictures need.
enum Shm {
    /// Not offered (a remote server, an old one), or it failed once.
    Off,
    Ready(Option<Segment>),
}

impl Shm {
    fn new(conn: &RustConnection) -> Self {
        let offered = has(conn, shm::X11_EXTENSION_NAME)
            && reply(conn.shm_query_version())
                .ok()
                .flatten()
                .is_some_and(|v| (v.major_version, v.minor_version) >= (1, 2));
        if offered {
            Self::Ready(None)
        } else {
            Self::Off
        }
    }

    fn usable(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    /// A segment of at least `bytes`, or `None` to read over the socket.
    fn segment(
        &mut self,
        conn: &RustConnection,
        bytes: usize,
    ) -> Result<Option<(u32, &Segment)>, ConnectionError> {
        let Self::Ready(current) = self else {
            return Ok(None);
        };
        if current.as_ref().is_none_or(|s| s.size < bytes) {
            if let Some(old) = current.take() {
                conn.shm_detach(old.seg)?.ignore_error();
            }
            // Room for a little more, so a window growing a few pixels
            // doesn't make a new one each frame.
            let size = bytes + bytes / 8;
            match Segment::new(conn, size)? {
                Some(segment) => *current = Some(segment),
                None => {
                    tracing::warn!(
                        "shared memory with the X server failed: pictures come over its socket"
                    );
                    *self = Self::Off;
                    return Ok(None);
                }
            }
        }
        let Self::Ready(Some(segment)) = self else {
            return Ok(None);
        };
        Ok(Some((segment.seg, segment)))
    }
}

/// A segment the server made, mapped read-only into this process.
struct Segment {
    seg: u32,
    memory: NonNull<u8>,
    size: usize,
    _fd: OwnedFd,
}

// The mapping is only read, and only after the server's reply to the
// request that wrote it: it moves between threads with the capture.
unsafe impl Send for Segment {}

impl Segment {
    fn new(conn: &RustConnection, size: usize) -> Result<Option<Self>, ConnectionError> {
        let Ok(size32) = u32::try_from(size) else {
            return Ok(None);
        };
        let seg = conn
            .generate_id()
            .map_err(|_| ConnectionError::UnknownError)?;
        // Not read-only to the server, which writes the pictures into it;
        // mapped read-only here.
        let made = match conn.shm_create_segment(seg, size32, false)?.reply() {
            Ok(made) => made,
            Err(ReplyError::X11Error(_)) => return Ok(None),
            Err(ReplyError::ConnectionError(e)) => return Err(e),
        };
        let fd = made.shm_fd;
        // SAFETY: a fresh shared mapping of the segment's file, `size` bytes,
        // read-only; nothing else in this process maps or frees it, and it
        // is unmapped once, in `drop`.
        let memory = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                size,
                rustix::mm::ProtFlags::READ,
                rustix::mm::MapFlags::SHARED,
                &fd,
                0,
            )
        };
        let Ok(memory) = memory else {
            conn.shm_detach(seg)?.ignore_error();
            return Ok(None);
        };
        let Some(memory) = NonNull::new(memory.cast::<u8>()) else {
            return Ok(None);
        };
        Ok(Some(Self {
            seg,
            memory,
            size,
            _fd: fd,
        }))
    }

    /// The first `bytes` of the segment, copied out. Only after the server
    /// answered the request that wrote them: it writes nothing meanwhile.
    fn read(&self, bytes: usize) -> Vec<u8> {
        let bytes = bytes.min(self.size);
        // SAFETY: `memory` maps `size` readable bytes until `drop`, and
        // `bytes <= size`; the server wrote them before replying and writes
        // nothing until the next request.
        unsafe { std::slice::from_raw_parts(self.memory.as_ptr(), bytes) }.to_vec()
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // SAFETY: the mapping `new` made, unmapped once; nothing borrows it
        // (`read` copies out).
        let _ = unsafe { rustix::mm::munmap(self.memory.as_ptr().cast(), self.size) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_premultiplied_pointer_comes_straight() {
        // Opaque red, half-transparent premultiplied grey, clear.
        let pixels = [0xffff_0000, 0x8040_4040, 0x0000_0000];
        let shape = shape(3, 1, (0, 0), &pixels).expect("a shape");
        let mut picture = vec![0u8; 3 * 4];
        cursor::draw(&mut picture, 3, 1, &shape, (0, 0)).expect("drawn");
        // Red over black is red; 50% straight grey (0x80) over black is
        // about 0x40; clear leaves black.
        assert_eq!(&picture[0..3], &[0, 0, 255]);
        assert!(picture[4..7].iter().all(|&c| (0x3f..=0x41).contains(&c)));
        assert_eq!(&picture[8..11], &[0, 0, 0]);
        // Too few pixels for its size: none.
        assert!(super::shape(2, 2, (0, 0), &pixels).is_none());
    }

    use super::super::testing::TestWindow;

    /// The next frame, within a few seconds.
    fn frame(capture: &mut X11Capture) -> BgraFrame {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match capture
                .next_event(Duration::from_millis(200))
                .expect("captures")
            {
                Some(CaptureEvent::Frame(VideoFrame::Cpu(frame))) => return frame,
                Some(other) => panic!("not a frame: {other:?}"),
                None => {}
            }
        }
        panic!("no frame came");
    }

    /// Whether a picture has more than one colour: something drawn.
    fn primary_screen() -> u32 {
        super::super::screens()
            .expect("screens listed")
            .into_iter()
            .find(|s| s.primary)
            .expect("a main screen")
            .id
    }

    fn drawn(frame: &BgraFrame) -> bool {
        let first = &frame.data[..3];
        frame.data.chunks(4).any(|p| &p[..3] != first)
    }

    /// Needs an X server with a window manager and `xclock`: in WSL, Xvfb
    /// with openbox (`Xvfb :71 &`, `DISPLAY=:71 openbox &`), `WAYLAND_DISPLAY`
    /// unset; with a compositor too (`DISPLAY=:71 xcompmgr &`) for the
    /// window's own picture. Run with `--ignored x11::`.
    #[test]
    #[ignore = "needs an X server, a window manager and xclock"]
    fn a_window_and_a_screen_are_captured_and_a_window_closing_ends_it() {
        assert!(
            super::super::session(),
            "an X11 session (WAYLAND_DISPLAY unset)"
        );
        let (window, id) = TestWindow::open("couch-x11-test");
        let mut capture = start(Target::Window(id), 30).expect("the window's capture starts");
        let picture = frame(&mut capture);
        assert_eq!((picture.width, picture.height), (300, 200));
        assert_eq!(picture.data.len(), 300 * 200 * 4);
        assert!(drawn(&picture), "the clock's face is in the picture");
        assert!(
            capture.shm.usable(),
            "the picture came through shared memory"
        );
        // Its own picture, compositor or not (redirected by the capture).
        assert_eq!(capture.redirected, Some(id));
        assert_eq!(
            capture.own_picture,
            Some(true),
            "the frames came from the window's own picture"
        );
        // A red window opened over it never shows in its frames (M28's
        // first run: with no compositor, windows over the game did).
        {
            let (_over, _) = TestWindow::open_with("couch-x11-over", &["-bg", "red"]);
            std::thread::sleep(Duration::from_millis(500));
            let covered = frame(&mut capture);
            assert!(
                !covered.data.chunks(4).any(|p| p[..3] == [0, 0, 255]),
                "the red window over it is in its picture"
            );
            let mut whole = start(Target::Screen(primary_screen()), 30).expect("the screen");
            let screen = frame(&mut whole);
            assert!(
                screen.data.chunks(4).any(|p| p[..3] == [0, 0, 255]),
                "the red window is on the screen (the check sees red)"
            );
        }
        // The same over the socket.
        capture.shm = Shm::Off;
        let socket = frame(&mut capture);
        assert_eq!((socket.width, socket.height), (300, 200));
        assert!(drawn(&socket));
        assert!(super::super::tracker::exists(id));

        let screens = super::super::screens().expect("screens listed");
        let screen = screens.iter().find(|s| s.primary).expect("a main screen");
        let mut whole = start(Target::Screen(screen.id), 30).expect("the screen's capture starts");
        let picture = frame(&mut whole);
        assert_eq!(
            (picture.width, picture.height),
            (screen.width, screen.height)
        );
        assert!(drawn(&picture), "the clock is on the screen");

        drop(window);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match capture
                .next_event(Duration::from_millis(200))
                .expect("ends cleanly")
            {
                Some(CaptureEvent::SourceClosed) => break,
                _ => assert!(
                    Instant::now() < deadline,
                    "the closed window never ended the capture"
                ),
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while super::super::tracker::exists(id) {
            assert!(
                Instant::now() < deadline,
                "the tracker still sees the closed window"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            start(Target::Window(id), 30).is_err(),
            "a gone window can't be captured"
        );
    }

    /// A window as a game makes it: `pid` its `_NET_WM_PID`, green, on a
    /// connection of its own (dropping it destroys the window).
    fn game_window(pid: u32) -> (RustConnection, u32) {
        use x11rb::{
            protocol::xproto::{CreateWindowAux, PropMode, WindowClass},
            wrapper::ConnectionExt as _,
        };
        let (conn, root) = super::super::connect().expect("the X server");
        let id = conn.generate_id().expect("an id");
        conn.create_window(
            0,
            id,
            root,
            700,
            300,
            200,
            150,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().background_pixel(0x00ff00),
        )
        .expect("sent")
        .check()
        .expect("made");
        let atom = conn
            .intern_atom(false, b"_NET_WM_PID")
            .expect("sent")
            .reply()
            .expect("an atom")
            .atom;
        conn.change_property32(
            PropMode::REPLACE,
            id,
            atom,
            xproto::AtomEnum::CARDINAL,
            &[pid],
        )
        .expect("sent");
        conn.map_window(id).expect("sent");
        conn.flush().expect("flushed");
        (conn, id)
    }

    /// SuperTuxKart switching full screen (M28's VM run) closes its X
    /// connection and opens a new one, whose window gets the same id: the
    /// capture follows it, redirected anew. The same id from another
    /// process is never captured. Skipped where the server gives another
    /// id. Needs an X server; run with `--ignored x11::`.
    #[test]
    #[ignore = "needs an X server"]
    fn a_window_back_by_its_id_is_followed_only_for_the_same_game() {
        // Green but for the pointer, which may lie over it.
        let green = |f: &BgraFrame| {
            let n = f.data.chunks(4).filter(|p| p[..3] == [0, 255, 0]).count();
            n * 10 > f.data.len() / 4 * 9
        };
        let ours = std::process::id();
        let (first, id) = game_window(ours);
        let mut capture = start(Target::Window(id), 30).expect("captures");
        let picture = frame(&mut capture);
        assert_eq!((picture.width, picture.height), (200, 150));
        assert!(green(&picture));

        // The game reopens its connection and its window.
        drop(first);
        std::thread::sleep(Duration::from_millis(300));
        let (second, again) = game_window(ours);
        if again != id {
            eprintln!("the server gave the new window another id: nothing to try");
            return;
        }
        let picture = frame(&mut capture);
        assert!(green(&picture), "the same game's window followed");
        assert_eq!(capture.redirected, Some(id), "redirected anew");

        // Another process's window by that id: the capture ends.
        drop(second);
        std::thread::sleep(Duration::from_millis(300));
        let (_third, third) = game_window(ours + 1);
        if third != id {
            eprintln!("the server gave the third window another id");
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match capture
                .next_event(Duration::from_millis(200))
                .expect("ends cleanly")
            {
                Some(CaptureEvent::SourceClosed) => break,
                Some(CaptureEvent::Frame(_)) => panic!("another process's window captured"),
                _ => assert!(Instant::now() < deadline, "the capture never ended"),
            }
        }
    }
}
