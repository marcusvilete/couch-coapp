//! The host's own stream window (M27d, ADR 0013): the stream as those
//! watching see it, at this PC, where the host sees and draws on the board
//! when nothing of ours can be over the game (a game in exclusive full
//! screen, a Linux host).
//!
//! - **Opened only on purpose:** the console's button, or the host's
//!   drawing key over a game in exclusive full screen (the author's choice:
//!   in front, in drawing mode; as that drawing ends the game comes back to
//!   the front). Never from afar: nobody would see it, and it costs an
//!   encode.
//! - **No seat and no input path:** its input is dropped unread
//!   (`pipeline::own_window`); what it draws goes into the board as the
//!   host's own, under the same checks as the board over the game's.
//! - **One author, two windows:** the board over the game and this window
//!   both draw as the host, so each hears the other's batches
//!   ([`SessionEffect::HostDrew`]), never its own, and both hear whatever
//!   the session tells the host ([`Actor::to_host_windows`]).

use std::time::{Duration, Instant};

use couch_proto::{
    board::BoardOp,
    ids::{RoomId, SessionId},
    ipc::{BoardNews, HudBoard, HudRole, HudState, ShortcutKeys, WindowRequest},
};
use couch_room::session::Effect as SessionEffect;

use super::{
    Actor, Hosting,
    board_overlay::{game_window, lies_over},
};
use crate::{
    EngineError, Event, OwnWindowView, Response,
    pipeline::{
        Backend, ViewerOptions,
        own_window::{OwnEvent, OwnWindow},
    },
    rooms::Input,
};

/// The host's own stream window's size, where the monitor allows.
const SIZE: (u32, u32) = (1280, 720);

/// Our own stream window, while open.
pub(in crate::rooms) struct Own {
    window: OwnWindow,
    /// Which of the session's own windows this is (`Input::Own`).
    number: u32,
    /// In drawing mode, as it last said.
    drawing: bool,
    /// When the host's key over a game in exclusive full screen asked it to
    /// draw: drawing that begins within [`BY_KEY_WAIT`] is the key's.
    asked_by_key: Option<Instant>,
    /// Its drawing now is the key's: as it ends, the game comes back.
    by_key: bool,
    /// To be brought forward by the engine once it shows its window: from a
    /// process not in front (the key comes through the hook while the game
    /// is), the window's own raise is refused.
    raise: bool,
    /// It has said how it is at least once: its loop runs.
    heard: bool,
}

/// How long drawing asked by the key may take to begin (the window opening,
/// then waiting for its first picture) and still count as the key's.
const BY_KEY_WAIT: Duration = Duration::from_secs(5);

/// Which of the host's two windows drew a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::rooms) enum Editor {
    /// The board over the game (M27b).
    Overlay,
    /// The host's own stream window.
    Own,
}

/// Closes `h`'s own stream window, if open: its feed stops, and drawing the
/// key began gives the game back the front. Also at the session's end, after
/// input is released and the end is sent.
pub(super) fn close_own_in(h: &mut Hosting, backend: &dyn Backend) {
    let Some(own) = h.own.take() else {
        return;
    };
    if let Some(pipeline) = &h.pipeline {
        pipeline.close_own();
    }
    let back = (own.by_key && own.drawing)
        .then(|| game_window(h))
        .flatten();
    // Dropped, it closes the window, letting it finish first.
    drop(own.window);
    tracing::info!("the host's own stream window closes");
    if let Some(game) = back {
        backend.bring_to_front(game);
    }
}

impl Actor {
    /// [`crate::Command::OwnStreamWindow`].
    pub(in crate::rooms) fn own_stream_window(
        &mut self,
        open: bool,
        from_afar: bool,
        now: Instant,
    ) -> Result<Response, EngineError> {
        if from_afar {
            return Err(EngineError::Refused(
                "your own stream window opens at the PC itself",
            ));
        }
        let h = self
            .sessions
            .hosting
            .as_ref()
            .filter(|h| h.pipeline.is_some())
            .ok_or(EngineError::NotLive)?;
        let room = h.room;
        match (&h.own, open) {
            (Some(own), true) => {
                own.window.window(WindowRequest::Raise);
                return Ok(Response::Done);
            }
            (None, true) => self.open_own(false, now)?,
            (Some(_), false) => self.close_own(now),
            (None, false) => {}
        }
        self.announce(room, now);
        Ok(Response::Done)
    }

    /// Opens our own stream window: in front and drawing with `by_key`.
    pub(in crate::rooms) fn open_own(
        &mut self,
        by_key: bool,
        now: Instant,
    ) -> Result<(), EngineError> {
        let backend = self.backend.clone();
        let data_dir = self.data_dir.clone();
        let inbox = self.inbox.clone();
        let h = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.pipeline.is_some() && h.own.is_none())
            .ok_or(EngineError::NotLive)?;
        let pipeline = h.pipeline.as_ref().expect("filtered");
        let frames = pipeline
            .open_own()
            .ok_or(EngineError::Refused("your stream window couldn't open"))?;
        // Beside the game, on another monitor, where there is one.
        let place = backend
            .own_window_area(lies_over(h).or_else(|| h.overlay.source.clone()).as_ref())
            .map(|a| {
                let width = SIZE.0.min(a.width.max(1) as u32);
                let height = SIZE.1.min(a.height.max(1) as u32);
                let x = a.x + (a.width - width as i32) / 2;
                let y = a.y + (a.height - height as i32) / 2;
                ((x, y), (width, height))
            });
        let options = ViewerOptions {
            log: Some(data_dir.join("viewer.log")),
            hide_stats: true,
            position: place.map(|(at, _)| at),
            size: place.map(|(_, size)| size),
            own_stream: true,
            ..ViewerOptions::default()
        };
        let shortcuts = ShortcutKeys {
            keys: crate::store::shortcuts(&data_dir).changed(),
        };
        let (window, mut events) = match OwnWindow::start(backend, frames, &options, shortcuts) {
            Ok(started) => started,
            Err(e) => {
                pipeline.close_own();
                return Err(EngineError::Media(e.to_string()));
            }
        };
        h.own_windows += 1;
        let number = h.own_windows;
        let session = h.session.id();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let input = Input::Own {
                    session,
                    window: number,
                    event,
                };
                if inbox.send(input).await.is_err() {
                    return;
                }
            }
        });
        tracing::info!(by_key, "the host's own stream window opens");
        // The board whole, to this window alone, in the host's generation as
        // it is: a new one would make the board over the game's batch on
        // its way stale.
        for news in h.session.host_snapshot() {
            window.board(news);
        }
        // And its pictures (M27e): a window that opens has none.
        for (picture, bytes) in h.session.host_pictures() {
            window.picture(couch_proto::ipc::Picture {
                picture,
                bytes: bytes.to_vec(),
            });
        }
        h.own_gate.reset();
        h.own = Some(Own {
            window,
            number,
            drawing: false,
            asked_by_key: None,
            by_key: false,
            raise: false,
            heard: false,
        });
        self.send_own_hud();
        if by_key {
            self.own_draws_by_key(now);
        }
        Ok(())
    }

    /// The host's key over a game in exclusive full screen: our own window
    /// comes to the front, in drawing mode (the author's choice).
    pub(in crate::rooms) fn own_draws_by_key(&mut self, now: Instant) {
        if let Some(own) = self.sessions.hosting.as_mut().and_then(|h| h.own.as_mut()) {
            if own.drawing {
                // Drawing already (opened from the console): the key's from
                // now on, so its end gives the game back.
                own.by_key = true;
            } else {
                own.asked_by_key = Some(now);
            }
            own.raise = true;
            own.window.window(WindowRequest::Drawing(true));
        }
        self.raise_own();
    }

    /// Brings our own window forward from here, with the key tap Windows
    /// asks of a process not in front, once it shows its window; until then
    /// it stays asked for.
    fn raise_own(&mut self) {
        let Some(own) = self
            .sessions
            .hosting
            .as_mut()
            .and_then(|h| h.own.as_mut())
            .filter(|o| o.raise)
        else {
            return;
        };
        let Some(window) = own
            .window
            .pid()
            .and_then(|pid| self.backend.window_of_process(pid))
        else {
            return;
        };
        own.raise = false;
        if !self.backend.bring_to_front(window) {
            tracing::warn!("Windows wouldn't bring the host's own stream window forward");
        }
    }

    /// Closes our own stream window, if open; drawing begun by the key gives
    /// the game back the front.
    pub(in crate::rooms) fn close_own(&mut self, _now: Instant) {
        if let Some(h) = self.sessions.hosting.as_mut() {
            close_own_in(h, &*self.backend);
        }
    }

    /// What our own stream window reports.
    pub(in crate::rooms) fn own_event(
        &mut self,
        session: SessionId,
        window: u32,
        event: OwnEvent,
        now: Instant,
    ) {
        let Some(h) = self.sessions.hosting.as_mut().filter(|h| {
            h.session.id() == session && h.own.as_ref().is_some_and(|o| o.number == window)
        }) else {
            return;
        };
        let room = h.room;
        match event {
            OwnEvent::Draw(draw) => {
                if let Err(e) = self.host_drew(Editor::Own, draw.generation, draw.ops, now) {
                    tracing::debug!("the host's own window's drawing: {e}");
                }
            }
            OwnEvent::Picture(up) => {
                if let Err(e) = self.host_picture(Editor::Own, up, now) {
                    tracing::debug!("the host's own window's picture: {e}");
                }
            }
            OwnEvent::KeyframeNeeded => {
                if let Some(pipeline) = &h.pipeline {
                    pipeline.own_keyframe();
                }
            }
            OwnEvent::Window(state) => {
                let back = game_window(h);
                let Some(own) = h.own.as_mut() else {
                    return;
                };
                // Its first word: the window runs, and only now waits for its
                // picture; the key's allowance counts from here, not from
                // a start that may have been slow.
                if !std::mem::replace(&mut own.heard, true) && own.asked_by_key.is_some() {
                    own.asked_by_key = Some(now);
                }
                let was = std::mem::replace(&mut own.drawing, state.drawing);
                if state.drawing && !was {
                    // The key's, if it asked for it a moment ago; a request
                    // the window turned down counts for nothing later.
                    own.by_key = own
                        .asked_by_key
                        .take()
                        .is_some_and(|asked| now.duration_since(asked) < BY_KEY_WAIT);
                }
                // Drawing the key began is over: back to the game.
                if !state.drawing
                    && was
                    && std::mem::take(&mut own.by_key)
                    && let Some(game) = back
                    && !self.backend.bring_to_front(game)
                {
                    tracing::warn!("Windows wouldn't bring the game back to the front");
                }
                // The window says how it is as soon as it shows: it can be
                // brought forward now, if the key asked.
                self.raise_own();
                if was != state.drawing {
                    self.announce(room, now);
                }
            }
            OwnEvent::Closed | OwnEvent::Gone => {
                if matches!(event, OwnEvent::Gone) {
                    tracing::warn!("the host's own stream window went");
                }
                self.close_own(now);
                self.announce(room, now);
            }
        }
    }

    /// The host's own batch, from one of its two windows: the same checks
    /// as anyone's; taken, the other window hears it (M27d).
    pub(in crate::rooms) fn host_drew(
        &mut self,
        from: Editor,
        generation: u32,
        ops: Vec<BoardOp>,
        now: Instant,
    ) -> Result<Response, EngineError> {
        if !couch_proto::board::valid_batch(&ops) {
            return Err(EngineError::Refused("that drawing isn't well formed"));
        }
        let me = self.me;
        let h = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.pipeline.is_some())
            .ok_or(EngineError::NotLive)?;
        // Behind a picture of this window's on its way (M27e): after it.
        let gate = h.gate_mut(from);
        if gate.busy() {
            gate.hold(super::pictures::Held::Draw(couch_proto::ipc::Draw {
                generation,
                ops,
            }));
            return Ok(Response::Done);
        }
        let fx = h.session.draw(me, generation, ops, now);
        self.host_effects(from, fx, now);
        Ok(Response::Done)
    }

    /// What the session says of a batch or picture of the host's, from
    /// window `from`: taken, the other window hears it, never `from`;
    /// refused, `from` alone hears why; the rest as any session effect.
    pub(in crate::rooms) fn host_effects(
        &mut self,
        from: Editor,
        fx: Vec<SessionEffect>,
        now: Instant,
    ) {
        let Some(room) = self.sessions.hosting.as_ref().map(|h| h.room) else {
            return;
        };
        let mut rest = Vec::new();
        for effect in fx {
            match effect {
                // Taken: to the other window, never back.
                SessionEffect::HostDrew(news) => {
                    let other = match from {
                        Editor::Overlay => Editor::Own,
                        Editor::Own => Editor::Overlay,
                    };
                    self.to_host_window(other, room, news);
                }
                // Refused: to the window that drew it alone; its resync, at
                // the tick, to both.
                SessionEffect::HostBoard(news @ BoardNews::Refused(_)) => {
                    self.to_host_window(from, room, news);
                }
                effect => rest.push(effect),
            }
        }
        self.apply_session(room, rest, now);
    }

    /// To one of the host's two windows.
    fn to_host_window(&self, to: Editor, room: RoomId, news: BoardNews) {
        match to {
            Editor::Own => {
                if let Some(own) = self.sessions.hosting.as_ref().and_then(|h| h.own.as_ref()) {
                    own.window.board(news);
                }
            }
            Editor::Overlay => self.to_overlay(room, news),
        }
    }

    /// A picture on the board, to both of the host's windows (M27e).
    pub(in crate::rooms) fn to_host_windows_picture(
        &self,
        room: RoomId,
        picture: couch_proto::board::PictureId,
        bytes: &[u8],
    ) {
        if let Some(own) = self.sessions.hosting.as_ref().and_then(|h| h.own.as_ref()) {
            own.window.picture(couch_proto::ipc::Picture {
                picture,
                bytes: bytes.to_vec(),
            });
        }
        if self.board_window_open() {
            let _ = self.events.send(Event::HostPicture {
                room: room.to_hex(),
                picture: crate::board::picture_hex(&picture),
                data: data_encoding::BASE64.encode(bytes),
            });
        }
    }

    /// What the session tells the host, to both of its windows.
    pub(in crate::rooms) fn to_host_windows(&self, room: RoomId, news: BoardNews) {
        if let Some(own) = self.sessions.hosting.as_ref().and_then(|h| h.own.as_ref()) {
            own.window.board(news.clone());
        }
        self.to_overlay(room, news);
    }

    /// To the board over the game, while its window is there: one that
    /// opens asks for the board whole.
    fn to_overlay(&self, room: RoomId, news: BoardNews) {
        if self.board_window_open() {
            let _ = self.events.send(Event::HostBoard {
                room: room.to_hex(),
                news: (&news).into(),
            });
        }
    }

    /// Our own stream window's HUD: the session as those watching see it,
    /// with no seats (it asks for none) and the host drawing as itself. The
    /// window's task drops a HUD it was already given.
    pub(in crate::rooms) fn send_own_hud(&self) {
        let Some(h) = self.sessions.hosting.as_ref() else {
            return;
        };
        let Some(own) = &h.own else {
            return;
        };
        let s = &h.session;
        own.window.set_hud(HudState {
            title: s.config().title.clone(),
            host: self.member_name(h.room, self.me),
            role: HudRole::Watching,
            seats: Vec::new(),
            host_on_seats: false,
            viewers: s.viewers().len().min(255) as u8,
            control_paused: false,
            game_in_front: true,
            stream: s.stream(),
            note: String::new(),
            board: HudBoard {
                on: s.board().is_on(),
                pictures: s.board().pictures_on(),
                // The host draws nothing while desktop control lasts (the
                // device's mouse would draw as the host): the window refuses
                // up front, with its own words for it.
                stopped: s.desktop().is_some(),
                me: Some(self.me),
                names: self
                    .rooms
                    .get(&h.room)
                    .map(|e| {
                        e.room
                            .status()
                            .members
                            .into_iter()
                            .map(|m| (m.id, m.name))
                            .collect()
                    })
                    .unwrap_or_default(),
            },
        });
    }

    /// Whether our own stream window is in front (the drawing key acts on
    /// it then).
    pub(in crate::rooms) fn own_in_front(&self) -> bool {
        let own = self.sessions.hosting.as_ref().and_then(|h| h.own.as_ref());
        own.and_then(|o| o.window.pid())
            .is_some_and(|pid| self.backend.foreground_process() == Some(pid))
    }

    /// The host's key with our own window in front: its drawing mode on or
    /// off, as its own key would.
    pub(in crate::rooms) fn own_drawing_key(&mut self) {
        if let Some(own) = self.sessions.hosting.as_ref().and_then(|h| h.own.as_ref()) {
            own.window.window(WindowRequest::Drawing(!own.drawing));
        }
    }

    /// The console's view of our own stream window.
    pub(super) fn own_window_view(&self, h: &Hosting) -> OwnWindowView {
        OwnWindowView {
            open: h.own.is_some(),
            drawing: h.own.as_ref().is_some_and(|o| o.drawing),
            left_out: self.backend.own_window_left_out(),
        }
    }
}
