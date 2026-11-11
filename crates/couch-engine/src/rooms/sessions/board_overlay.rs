//! The host's board overlay over the game (M27b, ADR 0013; Windows): what
//! is drawn on the stream, shown at this PC over the captured window, and
//! the host's own drawing there.
//!
//! - **The window is the app's to make, ours to place and switch**
//!   ([`crate::board::BoardWindows`], `Backend::place_board` and
//!   `board_interactive`): asked for while drawing is on, a stream is live
//!   and the host shows the drawings (or draws), closed otherwise.
//! - **A follower** lays it over exactly what is captured every 33 ms, and
//!   keeps it off every screen while the game isn't in front, while a game
//!   is in exclusive full screen (any window over one makes it leave full
//!   screen), and while nothing is drawn: no window of ours over the game
//!   for nothing.
//! - **Drawing over the game** is the one time a window of ours takes the
//!   focus from the game (the M8 rule's one exception): only by the host's
//!   own key at the PC (the low-level hook, which ignores injected keys),
//!   never from afar or from the webview; refused while drawing is off,
//!   nothing is streamed, desktop control lasts, or a game is in exclusive
//!   full screen. The game isn't in front meanwhile, so a guest on the
//!   keyboard seat is held off and its keys released, as with any change of
//!   the front. It ends with the key again, the overlay's Esc or Done, the
//!   focus going elsewhere, the panic key, and whenever it would now be
//!   refused; then the game comes back to the front (unless the focus went
//!   elsewhere on purpose).

use std::{
    any::Any,
    sync::Arc,
    time::{Duration, Instant},
};

use couch_input::shortcuts::Shortcut;
use couch_proto::{board::BoardOp, ids::SessionId};
use tokio::sync::{mpsc, watch};

use super::{Actor, OurStream, own_window::Editor};
use crate::{
    BoardOverlayView, EngineError, Response, ScreenArea,
    board::{BoardOpView, BoardWindows},
    pipeline::{Backend, HostKeyEvent, NativeId, Source},
    rooms::Input,
};

/// How often the follower lays the board over the game.
const FOLLOW_EVERY: Duration = Duration::from_millis(33);
/// How often it asks whether a game is in exclusive full screen.
const FULL_SCREEN_EVERY: Duration = Duration::from_millis(500);
/// It places the board again this often even when nothing seems to have
/// changed (a DPI change may have resized it).
const PLACE_AGAIN_EVERY: Duration = Duration::from_secs(1);
/// Drawing ends when another window has had the focus this long: the
/// moment the board takes it isn't counted.
const FOCUS_GRACE: Duration = Duration::from_millis(700);

/// Our session's board overlay.
pub(super) struct BoardOverlay {
    /// The host's switch: drawings shown over the game.
    pub shown: bool,
    /// Drawing over the game now.
    pub drawing: bool,
    /// The window, once made, and the app's side that made it, which closes
    /// it when this goes.
    window: Option<(NativeId, Arc<dyn BoardWindows>)>,
    /// Asked for and not yet made.
    opening: bool,
    /// The last try to make it failed: no other until the host asks again
    /// (the key, the switch) or a new stream starts.
    pub failed: bool,
    /// Its page has loaded (it asked for the board): until then it would
    /// show the webview's white, so it stays off screen.
    loaded: bool,
    /// Drawing has given it the focus.
    focused: bool,
    /// The window in front when drawing began, given the front back when
    /// it ends if the game isn't a window of its own (a screen streamed).
    before: Option<NativeId>,
    follow: Option<Follow>,
    /// The source of the stream now; for a window, the pipeline's own
    /// handle is used, which follows the game's window swaps.
    pub source: Option<Source>,
    /// The host's drawing key, as shown; `None` if it isn't hooked.
    pub key: Option<String>,
    /// Why the drawing key last did nothing, for the console.
    note: Option<&'static str>,
    /// The hook with only the drawing key, for a session with no seats
    /// (M24's one exception, the author's choice): while drawing is on.
    draw_key: Option<Box<dyn Any + Send>>,
}

impl Default for BoardOverlay {
    fn default() -> Self {
        Self {
            shown: true,
            drawing: false,
            window: None,
            opening: false,
            failed: false,
            loaded: false,
            focused: false,
            before: None,
            follow: None,
            source: None,
            key: None,
            note: None,
            draw_key: None,
        }
    }
}

impl Drop for BoardOverlay {
    fn drop(&mut self) {
        // The follower first, so it places nothing after the window goes.
        self.follow = None;
        if let Some((board, windows)) = self.window.take() {
            close(windows, board);
        }
    }
}

/// Closes the window `board` off whatever thread we are on (closing waits
/// on the app's own loop): that one alone, never a newer one made since.
fn close(windows: Arc<dyn BoardWindows>, board: NativeId) {
    let _ = std::thread::Builder::new()
        .name("board-close".into())
        .spawn(move || windows.close(board));
}

/// What the follower works from, as the actor last said.
#[derive(Debug, Clone, PartialEq)]
struct Target {
    board: NativeId,
    /// What is captured: the game's window, or a screen. `None` while
    /// nothing is streamed.
    source: Option<Source>,
    loaded: bool,
    drawing: bool,
    /// Drawing has given the board the focus: only from then on does
    /// another window having it count (the page's load isn't the host
    /// going elsewhere).
    focused: bool,
    shown: bool,
    elements: bool,
    marks_until: Option<Instant>,
}

struct Follow {
    target: watch::Sender<Target>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Follow {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Lays the board over the game while it should show, and says when
/// drawing lost the focus. Ends when the actor drops the [`Follow`].
async fn follow(
    backend: Arc<dyn Backend>,
    mut target: watch::Receiver<Target>,
    session: SessionId,
    inbox: mpsc::Sender<Input>,
) {
    let mut placed: Option<(Option<ScreenArea>, Instant)> = None;
    // Unknown counts as full screen: nothing over a game before the answer.
    let mut full_screen: Option<(bool, Instant)> = None;
    let mut away_since: Option<Instant> = None;
    let mut told_hidden = false;
    let mut tick = tokio::time::interval(FOLLOW_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let t = target.borrow_and_update().clone();
        let now = Instant::now();
        // While drawing, asked every tick: a stale answer could end a drawing
        // just begun after a game left full screen.
        let full = match full_screen {
            Some((full, at)) if !t.drawing && now.duration_since(at) < FULL_SCREEN_EVERY => full,
            _ => {
                let full = backend.exclusive_full_screen();
                full_screen = Some((full, now));
                full
            }
        };
        let area = t.source.as_ref().and_then(|s| backend.board_area(s));
        let game_in_front = match &t.source {
            Some(Source::Window(window)) => backend.focus(*window).foreground,
            Some(_) => true,
            None => false,
        };
        let something = t.elements || t.marks_until.is_some_and(|until| now < until);
        let show = t.loaded && !full && (t.drawing || (t.shown && something && game_in_front));
        let want = area.filter(|_| show);
        let due = placed.is_none_or(|(was, at)| {
            was != want || (want.is_some() && now.duration_since(at) >= PLACE_AGAIN_EVERY)
        });
        if due {
            backend.place_board(t.board, want);
            placed = Some((want, now));
        }
        // The window went (closed, its webview crashed): the actor forgets
        // it, and opens another if it is still wanted.
        if !backend.window_exists(t.board) {
            let _ = inbox
                .send(Input::BoardWindowGone {
                    session,
                    window: t.board,
                })
                .await;
            return;
        }
        let drawing = t.drawing && t.focused;
        // Drawing, but the board can't show (the game minimized or gone, a
        // game in exclusive full screen): a board in front that nobody can
        // see would swallow every key. Said once per drawing.
        if drawing && want.is_none() {
            if !told_hidden {
                told_hidden = true;
                if inbox.send(Input::BoardHidden(session)).await.is_err() {
                    return;
                }
            }
        } else if !t.drawing {
            told_hidden = false;
        }
        // Drawing, and another window has the focus: the host went
        // elsewhere. Counted only once the board was given the focus.
        if drawing && !backend.focus(t.board).foreground {
            let since = *away_since.get_or_insert(now);
            if now.duration_since(since) >= FOCUS_GRACE {
                away_since = None;
                if inbox.send(Input::BoardFocusLost(session)).await.is_err() {
                    return;
                }
            }
        } else {
            away_since = None;
        }
    }
}

/// What the board lies over while a stream is live: the game's window as
/// the pipeline follows it (window swaps included), or the screen.
pub(super) fn lies_over(h: &super::Hosting) -> Option<Source> {
    if !matches!(h.stream, Some(OurStream::Live { .. })) {
        return None;
    }
    match &h.overlay.source {
        Some(Source::Window(_) | Source::WindowTitle(_)) => h
            .pipeline
            .as_ref()
            .and_then(|p| p.window())
            .map(Source::Window),
        other => other.clone(),
    }
}

/// The game's own window, if it is one.
pub(super) fn game_window(h: &super::Hosting) -> Option<NativeId> {
    match lies_over(h) {
        Some(Source::Window(window)) => Some(window),
        _ => None,
    }
}

impl Actor {
    fn draws_over_game(&self) -> bool {
        self.backend.draws_over_game() && self.board_windows.is_some()
    }

    /// Whether our board overlay's window is there. One still opening
    /// needs no news: its page asks for the board whole once loaded.
    pub(in crate::rooms) fn board_window_open(&self) -> bool {
        self.sessions
            .hosting
            .as_ref()
            .is_some_and(|h| h.overlay.window.is_some())
    }

    /// Our overlay as it should be now: after every change to our session,
    /// and every tick (the game's window may have been swapped, a mark's
    /// time be up).
    pub(in crate::rooms) fn sync_board_overlay(&mut self, now: Instant) {
        let can = self.draws_over_game();
        let Some(h) = self
            .sessions
            .hosting
            .as_ref()
            .filter(|h| h.pipeline.is_some())
        else {
            return;
        };
        let live = matches!(h.stream, Some(OurStream::Live { .. }));
        let on = h.session.board().is_on() && !h.session.is_ended();
        // Drawing can't outlast what lets it begin.
        if h.overlay.drawing && (!on || !live || h.session.desktop_holder().is_some()) {
            self.end_drawing(live, now);
        }
        let backend = self.backend.clone();
        let inbox = self.inbox.clone();
        let windows = self.board_windows.clone();
        let data_dir = self.data_dir.clone();
        let Some(h) = self.sessions.hosting.as_mut() else {
            return;
        };
        let session = h.session.id();
        let wanted = can && on && live && (h.overlay.shown || h.overlay.drawing);
        let source = lies_over(h);
        if wanted
            && h.overlay.window.is_none()
            && !h.overlay.opening
            && !h.overlay.failed
            && let Some(windows) = windows
        {
            h.overlay.opening = true;
            // Its page's word comes after this, and may come before the
            // actor hears the window was made.
            h.overlay.loaded = false;
            let inbox = inbox.clone();
            tokio::spawn(async move {
                let window = tokio::task::spawn_blocking(move || windows.open())
                    .await
                    .ok()
                    .flatten();
                let _ = inbox
                    .send(Input::BoardWindowOpened { session, window })
                    .await;
            });
        } else if !wanted && h.overlay.window.is_some() {
            h.overlay.follow = None;
            h.overlay.loaded = false;
            if let Some((board, windows)) = h.overlay.window.take() {
                backend.place_board(board, None);
                close(windows, board);
            }
        }
        if let Some((board, _)) = h.overlay.window.as_ref() {
            let target = Target {
                board: *board,
                source,
                drawing: h.overlay.drawing,
                focused: h.overlay.focused,
                loaded: h.overlay.loaded,
                shown: h.overlay.shown,
                elements: h.session.board_has_elements(),
                marks_until: h.session.marks_until(),
            };
            match &h.overlay.follow {
                Some(f) => {
                    f.target.send_if_modified(|t| {
                        let changed = *t != target;
                        *t = target;
                        changed
                    });
                }
                None => {
                    let (tx, rx) = watch::channel(target);
                    let task = tokio::spawn(follow(backend.clone(), rx, session, inbox.clone()));
                    h.overlay.follow = Some(Follow { target: tx, task });
                }
            }
        }
        // A session with no seats has no hook of the host's keys (M24): the
        // drawing key alone gets one, while drawing is on.
        let seatless = h.session.config().seats.is_empty();
        let want_key = seatless && can && on;
        if !want_key {
            if h.overlay.draw_key.take().is_some() {
                h.overlay.key = None;
            }
        } else if h.overlay.draw_key.is_none() {
            let binding = *crate::store::shortcuts(&data_dir).binding(Shortcut::HostDrawing);
            let inbox = self.inbox.clone();
            let runtime = tokio::runtime::Handle::current();
            let pressed = move |event: HostKeyEvent| {
                if event == HostKeyEvent::Pressed(Shortcut::HostDrawing) {
                    let inbox = inbox.clone();
                    runtime.spawn(async move {
                        let _ = inbox.send(Input::HostDrawKey(session)).await;
                    });
                }
            };
            let unattended = h.from_afar;
            match backend.host_keys(
                &[(binding.key, Shortcut::HostDrawing)],
                unattended,
                Box::new(pressed),
            ) {
                Ok(guard) => {
                    h.overlay.draw_key = Some(guard);
                    h.overlay.key = Some(binding.keys());
                }
                Err(e) => {
                    tracing::warn!("no drawing key in a session with no seats: {e}");
                    // Not again every change: once per time drawing is on.
                    h.overlay.draw_key = Some(Box::new(()));
                }
            }
        }
    }

    /// The board overlay's window was made, or couldn't be.
    pub(in crate::rooms) fn board_window_opened(
        &mut self,
        session: SessionId,
        window: Option<NativeId>,
        now: Instant,
    ) {
        let windows = self.board_windows.clone();
        let current = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.session.id() == session && !h.session.is_ended());
        let Some(h) = current else {
            // The session went meanwhile.
            if let (Some(board), Some(windows)) = (window, windows) {
                close(windows, board);
            }
            return;
        };
        h.overlay.opening = false;
        let (Some(board), Some(windows)) = (window, windows) else {
            tracing::warn!(
                "the board overlay's window couldn't be made; not again until the key, \
                 the switch or a new stream"
            );
            // Never tried again on every change: each try flashes a window.
            h.overlay.failed = true;
            h.overlay.drawing = false;
            h.overlay.note = Some("The drawing window couldn't be opened");
            let room = h.room;
            self.announce(room, now);
            return;
        };
        h.overlay.window = Some((board, windows));
        // It waits off screen, click-through, until its page has loaded and
        // the follower places it (its page may have said so already: the
        // two race); drawing takes the focus then.
        self.backend.board_interactive(board, false);
        self.sync_board_overlay(now);
        self.try_begin_drawing();
    }

    /// Drawing over the game, asked for, begins once the window's page has
    /// loaded: the board takes the mouse and the focus.
    fn try_begin_drawing(&mut self) {
        let Some(h) = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.overlay.drawing && h.overlay.loaded && !h.overlay.focused)
        else {
            return;
        };
        let Some(board) = h.overlay.window.as_ref().map(|(b, _)| *b) else {
            return;
        };
        h.overlay.focused = true;
        let area = h
            .overlay
            .follow
            .as_ref()
            .and_then(|f| f.target.borrow().source.clone())
            .and_then(|s| self.backend.board_area(&s));
        self.backend.board_interactive(board, true);
        self.backend.place_board(board, area);
        if !self.backend.bring_to_front(board) {
            tracing::warn!("Windows wouldn't give the board overlay the focus");
        }
    }

    /// Drawing over the game ends: the board lets every click through
    /// again, and with `bring_back` the game comes back to the front.
    fn end_drawing(&mut self, bring_back: bool, _now: Instant) {
        let Some(h) = self.sessions.hosting.as_mut().filter(|h| h.overlay.drawing) else {
            return;
        };
        h.overlay.drawing = false;
        h.overlay.focused = false;
        if let Some(f) = &h.overlay.follow {
            f.target.send_modify(|t| t.drawing = false);
        }
        let board = h.overlay.window.as_ref().map(|(b, _)| *b);
        // The game's window; on a screen streamed, whatever was in front
        // when drawing began, so the keys don't stay with a board that no
        // longer takes them.
        let before = h.overlay.before.take().filter(|w| Some(*w) != board);
        let back = game_window(h).or(before);
        if let Some(board) = board {
            self.backend.board_interactive(board, false);
        }
        if bring_back
            && let Some(back) = back
            && !self.backend.bring_to_front(back)
        {
            tracing::warn!("Windows wouldn't bring the game back to the front");
        }
    }

    /// Drawing over the game, and the board can't show (the game off
    /// screen, a game in exclusive full screen): it ends, the game brought
    /// back, rather than leave an unseen board taking every key.
    pub(in crate::rooms) fn board_hidden(&mut self, session: SessionId, now: Instant) {
        let Some(h) = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.session.id() == session && h.overlay.drawing)
        else {
            return;
        };
        h.overlay.note = Some("Drawing ended: the game went off screen");
        let room = h.room;
        self.end_drawing(true, now);
        self.announce(room, now);
    }

    /// The board's window is gone (closed, its webview crashed): forgotten,
    /// drawing ended, and another opened if one is still wanted.
    pub(in crate::rooms) fn board_window_gone(
        &mut self,
        session: SessionId,
        window: NativeId,
        now: Instant,
    ) {
        let Some(h) = self.sessions.hosting.as_mut().filter(|h| {
            h.session.id() == session
                && h.overlay.window.as_ref().is_some_and(|(b, _)| *b == window)
        }) else {
            return;
        };
        tracing::warn!("the board overlay's window went; opening another if still wanted");
        let room = h.room;
        let drawing = h.overlay.drawing;
        self.end_drawing(true, now);
        if let Some(h) = self.sessions.hosting.as_mut() {
            h.overlay.follow = None;
            h.overlay.loaded = false;
            if let Some((board, windows)) = h.overlay.window.take() {
                close(windows, board);
            }
        }
        self.sync_board_overlay(now);
        if drawing {
            self.announce(room, now);
        }
    }

    /// The host's drawing key, pressed at the PC: drawing over the game
    /// begins, or ends.
    pub(in crate::rooms) fn host_draw_key(&mut self, session: SessionId, now: Instant) {
        // Our own stream window in front (M27d): the key is its drawing key,
        // which the hook took before the window could hear it.
        let ours = self
            .sessions
            .hosting
            .as_ref()
            .is_some_and(|h| h.session.id() == session && h.pipeline.is_some());
        if ours && self.own_in_front() {
            self.own_drawing_key();
            return;
        }
        let can = self.draws_over_game();
        let full_screen = self.backend.exclusive_full_screen();
        let before = self.backend.foreground_window();
        let on_screen = self
            .sessions
            .hosting
            .as_ref()
            .and_then(lies_over)
            .and_then(|s| self.backend.board_area(&s))
            .is_some();
        let Some(h) = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.session.id() == session && h.pipeline.is_some())
        else {
            return;
        };
        let room = h.room;
        if h.overlay.drawing {
            self.end_drawing(true, now);
            self.announce(room, now);
            return;
        }
        let refused = if !can {
            Some("Drawing over the game needs Windows")
        } else if !h.session.board().is_on() {
            Some("Turn drawing on first")
        } else if !matches!(h.stream, Some(OurStream::Live { .. })) {
            Some("Nothing is streamed to draw on")
        } else if h.session.desktop_holder().is_some() {
            Some("Not while a device of yours controls this PC's desktop")
        } else {
            None
        };
        // Nothing of ours can be over a game in exclusive full screen: our
        // own stream window instead, in front, drawing (M27d, the author's
        // choice); as that drawing ends, the game comes back.
        if refused.is_none() && full_screen {
            h.overlay.failed = false;
            h.overlay.note = Some(
                "Drawings can't show over a game in exclusive full screen: you draw in your \
                 stream window, or set the game to borderless",
            );
            if h.own.is_some() {
                self.own_draws_by_key(now);
            } else if let Err(e) = self.open_own(true, now) {
                tracing::warn!("the host's own stream window didn't open: {e}");
                if let Some(h) = self.sessions.hosting.as_mut() {
                    h.overlay.note = Some("Your stream window couldn't open");
                }
            }
            self.announce(room, now);
            return;
        }
        let refused = refused.or_else(|| {
            (!on_screen).then_some("The game isn't on screen to draw over: bring it back first")
        });
        h.overlay.note = refused;
        // Asked again: a window that failed may be tried once more.
        h.overlay.failed = false;
        if refused.is_none() {
            h.overlay.drawing = true;
            h.overlay.before = before;
            // The window, if not made yet (the drawings hidden), opens now
            // and takes the focus once its page has loaded.
            self.sync_board_overlay(now);
            self.try_begin_drawing();
        } else {
            tracing::info!(why = refused, "the drawing key did nothing");
        }
        self.announce(room, now);
    }

    /// Another window has the focus while we draw: the host went there.
    pub(in crate::rooms) fn board_focus_lost(&mut self, session: SessionId, now: Instant) {
        let Some(room) = self
            .sessions
            .hosting
            .as_ref()
            .filter(|h| h.session.id() == session && h.overlay.drawing)
            .map(|h| h.room)
        else {
            return;
        };
        self.end_drawing(false, now);
        self.announce(room, now);
    }

    /// The panic key ends drawing too, the game back in front.
    pub(in crate::rooms) fn panic_ends_drawing(&mut self, now: Instant) {
        if let Some(room) = self
            .sessions
            .hosting
            .as_ref()
            .filter(|h| h.overlay.drawing)
            .map(|h| h.room)
        {
            self.end_drawing(true, now);
            self.announce(room, now);
        }
    }

    /// [`crate::Command::HostDraw`]: the host's own operations.
    pub(in crate::rooms) fn host_draw(
        &mut self,
        generation: u32,
        ops: &[BoardOpView],
        now: Instant,
    ) -> Result<Response, EngineError> {
        let ops: Vec<BoardOp> = ops.iter().map(Into::into).collect();
        // Our own stream window hears it too (M27d).
        self.host_drew(Editor::Overlay, generation, ops, now)
    }

    /// [`crate::Command::HostBoard`]: the board whole, for the overlay.
    /// The overlay's page asks for it once it has loaded: the window may
    /// show from now on, and take the focus if drawing waits for it.
    pub(in crate::rooms) fn host_board(&mut self, now: Instant) -> Result<Response, EngineError> {
        let reply = self.our_session(|s| Ok(s.host_board()), now)?;
        if let Some(h) = self.sessions.hosting.as_mut()
            && (h.overlay.window.is_some() || h.overlay.opening)
            && !h.overlay.loaded
        {
            h.overlay.loaded = true;
            self.sync_board_overlay(now);
            self.try_begin_drawing();
        }
        Ok(reply)
    }

    /// [`crate::Command::EndHostDrawing`].
    pub(in crate::rooms) fn end_host_drawing(
        &mut self,
        now: Instant,
    ) -> Result<Response, EngineError> {
        let room = self
            .sessions
            .hosting
            .as_ref()
            .filter(|h| h.pipeline.is_some())
            .map(|h| h.room)
            .ok_or(EngineError::NotLive)?;
        self.end_drawing(true, now);
        self.announce(room, now);
        Ok(Response::Done)
    }

    /// [`crate::Command::ShowBoardOverlay`].
    pub(in crate::rooms) fn show_board_overlay(
        &mut self,
        on: bool,
        now: Instant,
    ) -> Result<Response, EngineError> {
        let h = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.pipeline.is_some())
            .ok_or(EngineError::NotLive)?;
        h.overlay.shown = on;
        h.overlay.failed = false;
        let room = h.room;
        self.sync_board_overlay(now);
        self.announce(room, now);
        Ok(Response::Done)
    }

    /// The console's view of our overlay.
    pub(super) fn board_overlay_view(&self, overlay: &BoardOverlay) -> BoardOverlayView {
        BoardOverlayView {
            available: self.draws_over_game(),
            shown: overlay.shown,
            drawing: overlay.drawing,
            key: overlay.key.clone(),
            note: overlay.note.map(str::to_string),
        }
    }
}
