//! Pictures on the board (M27e, ADR 0013; docs/protocol.md, Drawing on the
//! stream): their bytes on picture streams of their own, never the control
//! stream; the host checks each before its board takes it.
//!
//! **A member's side:** its window's picture goes to the host on a stream of
//! its own, one at a time, and the host answers on the control stream
//! ([`PictureAnswer`]). Until then the member's later batches wait here
//! ([`Gate`]), so a picture pasted and then moved reaches the host in that
//! order, though its bytes and the move travel apart.
//!
//! **The host's side:** a viewer's pictures are read only from a viewer of
//! our session, on the connection it joined on, one at a time; each is
//! decoded with limits off the actor's thread (`couch_media::picture::check`)
//! before the session sees it. The board's pictures go to each viewer once,
//! on streams of their own ([`SessionEffect::SendPicture`]). The host's own
//! windows hand theirs in the same way, each behind a gate of its own.

use std::{collections::VecDeque, sync::Arc, time::Duration};

use couch_proto::{
    board::{
        ElementKind, PictureAnswer, PictureHeader, PictureId, PictureUpload, Refusal, valid_batch,
    },
    control::ControlMessage,
    ids::{MemberId, SessionId},
    ipc::{BoardNews, Draw, PictureUp},
    media::stream_kind,
};
use couch_room::board::PictureBytes;
use tokio::task::JoinHandle;

use super::{
    super::{Actor, ConnId, Input, Outgoing},
    own_window::Editor,
};
use crate::{EngineError, Response};

/// A picture sent and not answered after this long counts as answered, so a
/// lost answer never holds a member's drawing for good: longer than the host
/// gives a picture to arrive (`couch_net::pictures::READ_WITHIN`).
pub(super) const ANSWER_WITHIN: Duration = Duration::from_secs(75);

/// What a window drew, held behind a picture on its way.
#[derive(Debug)]
pub(in crate::rooms) enum Held {
    Draw(Draw),
    Picture(PictureUp),
}

/// One window's drawing in order across a picture on its way (M27e): while
/// a picture is sent (or checked, for the host's own) and not answered,
/// what the window draws next waits here.
#[derive(Debug, Default)]
pub(in crate::rooms) struct Gate {
    /// The upload on its way (its number), and since when.
    pending: Option<(u32, std::time::Instant)>,
    held: VecDeque<Held>,
}

/// The next upload's number (P17), one count for the whole process: across
/// resets, watches and windows, so an answer to an upload given up on
/// (overdue, on a connection gone, or of a watch ended) never stands for a
/// later one, of the same element or not.
static NEXT_UPLOAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl Gate {
    /// Something waits, or is on its way: what comes now must wait too.
    pub(in crate::rooms) fn busy(&self) -> bool {
        self.pending.is_some() || !self.held.is_empty()
    }

    pub(in crate::rooms) fn hold(&mut self, held: Held) {
        // A window can't make a host wait for ever: past this, its oldest
        // goes (the host's resync puts its board right).
        const MOST: usize = 256;
        if self.held.len() >= MOST {
            self.held.pop_front();
        }
        self.held.push_back(held);
    }

    /// An upload goes now: its number, waited on from `now`.
    fn start(&mut self, now: std::time::Instant) -> u32 {
        let upload = NEXT_UPLOAD.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.pending = Some((upload, now));
        upload
    }

    /// The answer for upload `upload`: whether it was the one on its way.
    fn answered(&mut self, upload: u32) -> bool {
        if self.pending.is_some_and(|(p, _)| p == upload) {
            self.pending = None;
            true
        } else {
            false
        }
    }

    /// The picture on its way waited too long.
    fn overdue(&self, now: std::time::Instant) -> bool {
        self.pending
            .is_some_and(|(_, since)| now.saturating_duration_since(since) >= ANSWER_WITHIN)
    }

    /// The next thing to send, unless a picture is on its way.
    fn next(&mut self) -> Option<Held> {
        if self.pending.is_some() {
            return None;
        }
        self.held.pop_front()
    }

    /// Everything dropped: a new connection, or a window gone.
    pub(in crate::rooms) fn reset(&mut self) {
        self.pending = None;
        self.held.clear();
    }
}

/// A task that ends with its owner.
#[derive(Debug)]
pub(in crate::rooms) struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A picture is what the board takes: its bytes decode within the limits,
/// and are what its element names. Off the actor's thread.
async fn checked(element: &couch_proto::board::Element, bytes: Vec<u8>) -> Option<Vec<u8>> {
    let named = element.picture?;
    tokio::task::spawn_blocking(move || {
        (couch_media::picture::picture_id(&bytes) == named
            && couch_media::picture::check(&bytes).is_ok())
        .then_some(bytes)
    })
    .await
    .ok()
    .flatten()
}

impl Actor {
    // ---- A member's side ----

    /// Our window's drawing, to the host: at once, or after the picture on
    /// its way.
    pub(super) fn watch_draw(&mut self, draw: Draw) {
        let Some(w) = self.sessions.watching.as_mut() else {
            return;
        };
        // Our own window's, but checked anyway: a batch too large for a
        // control frame would close the connection.
        if !w.joined() || !valid_batch(&draw.ops) {
            return;
        }
        if w.gate.busy() {
            w.gate.hold(Held::Draw(draw));
            return;
        }
        self.send_draw(draw);
    }

    fn send_draw(&self, draw: Draw) {
        let Some(w) = self.sessions.watching.as_ref() else {
            return;
        };
        let message = ControlMessage::BoardOps(couch_proto::board::BoardOps {
            session: w.session,
            generation: draw.generation,
            ops: draw.ops,
        });
        self.send(w.conn, Outgoing::Message(message));
    }

    /// Our window's picture, to the host, or held behind the one on its way.
    pub(super) fn watch_picture(&mut self, up: PictureUp, now: std::time::Instant) {
        let Some(w) = self.sessions.watching.as_mut() else {
            return;
        };
        if !w.joined() {
            return;
        }
        if w.gate.busy() {
            w.gate.hold(Held::Picture(up));
            return;
        }
        self.send_picture(up, now);
    }

    fn send_picture(&mut self, up: PictureUp, now: std::time::Instant) {
        let Some(w) = self.sessions.watching.as_mut() else {
            return;
        };
        let mut header = PictureUpload {
            session: w.session,
            upload: 0,
            generation: up.generation,
            element: up.element,
            len: up.bytes.len() as u32,
        };
        // Our window's own, but checked anyway: the host would only refuse
        // it, after its bytes.
        if !header.is_valid() {
            return;
        }
        let Some(c) = self.conns.get(&w.conn) else {
            return;
        };
        header.upload = w.gate.start(now);
        let conn = c.conn.clone();
        tokio::spawn(async move {
            if let Err(e) =
                couch_net::pictures::send(&conn, stream_kind::PICTURE_UP, &header, &up.bytes).await
            {
                tracing::debug!("a picture for the host: {e}");
            }
        });
    }

    /// The host answered our picture: its refusal to our window, and what
    /// waited behind it to the host.
    pub(super) fn picture_answered(&mut self, answer: PictureAnswer, now: std::time::Instant) {
        let Some(w) = self.sessions.watching.as_mut() else {
            return;
        };
        if w.session != answer.session || !w.gate.answered(answer.upload) {
            return;
        }
        // Stale: our window heard of the clear itself.
        if let Some(why) = answer.refusal.filter(|why| *why != Refusal::Stale) {
            self.to_window_board(BoardNews::Refused(why));
        }
        self.drain_watch(now);
    }

    fn drain_watch(&mut self, now: std::time::Instant) {
        loop {
            let Some(next) = self.sessions.watching.as_mut().and_then(|w| w.gate.next()) else {
                return;
            };
            match next {
                Held::Draw(draw) => self.send_draw(draw),
                Held::Picture(up) => {
                    self.send_picture(up, now);
                    if self
                        .sessions
                        .watching
                        .as_ref()
                        .is_some_and(|w| w.gate.pending.is_some())
                    {
                        return;
                    }
                }
            }
        }
    }

    // ---- The host's side ----

    /// Reads `member`'s pictures on the connection it joined our session on,
    /// one at a time, each checked off this thread before the session sees
    /// it. Only while it watches: the reader goes with it.
    pub(super) fn read_pictures(&mut self, member: MemberId, conn: ConnId) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        let Some(h) = self.sessions.hosting.as_mut() else {
            return;
        };
        let session = h.session.id();
        let mut streams = c.streams.subscribe(stream_kind::PICTURE_UP);
        let inbox = self.inbox.clone();
        let task = tokio::spawn(async move {
            while let Some(recv) = streams.recv().await {
                let Some((upload, body)) = couch_net::pictures::read_upload_header(recv).await
                else {
                    continue;
                };
                if upload.session != session {
                    body.refuse();
                    continue;
                }
                // The session first (P17): a picture it would refuse anyway
                // (pictures off, the member stopped or too fast) is answered
                // now, and its bytes are never read nor decoded.
                let (reply, coming) = tokio::sync::oneshot::channel();
                let ask = Input::PictureComing {
                    conn,
                    peer: member,
                    upload: upload.clone(),
                    reply,
                };
                if inbox.send(ask).await.is_err() {
                    return;
                }
                if !coming.await.unwrap_or(false) {
                    body.refuse();
                    continue;
                }
                // Bytes that don't come whole are answered as no picture,
                // so the member's gate isn't left waiting (P17's review).
                let bytes = match body.read().await {
                    Some(bytes) => checked(&upload.element, bytes).await,
                    None => None,
                };
                let input = Input::PictureUp {
                    conn,
                    peer: member,
                    upload,
                    bytes,
                };
                if inbox.send(input).await.is_err() {
                    return;
                }
            }
        });
        h.picture_readers.insert(member, AbortOnDrop(task));
    }

    /// A viewer's picture whose header alone was read (P17): whether to
    /// read its bytes. Refused, it is answered here, as the session says.
    pub(in crate::rooms) fn picture_coming(
        &mut self,
        conn: ConnId,
        peer: MemberId,
        upload: &PictureUpload,
        now: std::time::Instant,
    ) -> bool {
        let Some(h) = self.sessions.hosting.as_mut() else {
            return false;
        };
        if !h.session.is_viewer(peer) || h.viewer_conns.get(&peer) != Some(&conn) {
            return false;
        }
        let room = h.room;
        match h.session.picture_coming(peer, upload, now) {
            Ok(()) => true,
            Err(fx) => {
                self.apply_session(room, fx, now);
                false
            }
        }
    }

    /// A viewer's picture, read and checked: to the session, from the
    /// viewer itself on the connection it joined on.
    pub(in crate::rooms) fn picture_up(
        &mut self,
        conn: ConnId,
        peer: MemberId,
        upload: PictureUpload,
        bytes: Option<Vec<u8>>,
        now: std::time::Instant,
    ) {
        let Some(h) = self.sessions.hosting.as_mut() else {
            return;
        };
        if !h.session.is_viewer(peer) || h.viewer_conns.get(&peer) != Some(&conn) {
            return;
        }
        let room = h.room;
        let fx = match bytes {
            Some(bytes) => {
                let bytes: PictureBytes = Arc::from(bytes);
                h.session.put_picture(
                    peer,
                    upload.upload,
                    upload.generation,
                    upload.element,
                    &bytes,
                    now,
                )
            }
            None => h.session.refuse_picture(
                peer,
                upload.upload,
                upload.element.id,
                Refusal::BadPicture,
                now,
            ),
        };
        self.apply_session(room, fx, now);
    }

    /// A picture of the board's, to one viewer, on a stream of its own.
    pub(super) fn send_picture_to(
        &self,
        session: SessionId,
        to: MemberId,
        picture: PictureId,
        bytes: PictureBytes,
    ) {
        let Some(conn) = self
            .sessions
            .hosting
            .as_ref()
            .and_then(|h| h.viewer_conns.get(&to))
            .and_then(|id| self.conns.get(id))
            .map(|c| c.conn.clone())
        else {
            return;
        };
        let header = PictureHeader {
            session,
            picture,
            len: bytes.len() as u32,
        };
        tokio::spawn(async move {
            if let Err(e) =
                couch_net::pictures::send(&conn, stream_kind::PICTURE, &header, &bytes).await
            {
                tracing::debug!("a picture for a viewer: {e}");
            }
        });
    }

    // ---- The host's own windows ----

    /// The host's own picture from one of its windows: checked off this
    /// thread as a viewer's is, then to the session as the host's. What the
    /// window draws meanwhile waits behind it.
    pub(in crate::rooms) fn host_picture(
        &mut self,
        from: Editor,
        up: PictureUp,
        now: std::time::Instant,
    ) -> Result<Response, EngineError> {
        let h = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.pipeline.is_some())
            .ok_or(EngineError::NotLive)?;
        if up.element.kind != ElementKind::Picture
            || !up.element.is_valid()
            || up.bytes.is_empty()
            || up.bytes.len() > couch_proto::board::MAX_PICTURE_BYTES
        {
            return Err(EngineError::Refused("that picture isn't well formed"));
        }
        let gate = h.gate_mut(from);
        if gate.busy() {
            gate.hold(Held::Picture(up));
            return Ok(Response::Done);
        }
        self.check_host_picture(from, up, now);
        Ok(Response::Done)
    }

    fn check_host_picture(&mut self, from: Editor, up: PictureUp, now: std::time::Instant) {
        let Some(h) = self.sessions.hosting.as_mut() else {
            return;
        };
        let upload = h.gate_mut(from).start(now);
        let session = h.session.id();
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            let PictureUp {
                generation,
                element,
                bytes,
            } = up;
            let bytes = checked(&element, bytes).await;
            let _ = inbox
                .send(Input::HostPictureChecked {
                    session,
                    own: from == Editor::Own,
                    upload,
                    generation,
                    element,
                    bytes,
                })
                .await;
        });
    }

    /// The host's picture, checked: to the session, then what its window
    /// drew meanwhile.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::rooms) fn host_picture_checked(
        &mut self,
        session: SessionId,
        from: Editor,
        upload: u32,
        generation: u32,
        element: couch_proto::board::Element,
        bytes: Option<Vec<u8>>,
        now: std::time::Instant,
    ) {
        let me = self.me;
        let Some(h) = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.session.id() == session)
        else {
            return;
        };
        // Its own check alone (P17): one given up on after its wait counts
        // for nothing when it comes, whatever the window put since.
        if !h.gate_mut(from).answered(upload) {
            return;
        }
        let fx = match bytes {
            Some(bytes) => {
                let bytes: PictureBytes = Arc::from(bytes);
                h.session
                    .put_picture(me, upload, generation, element, &bytes, now)
            }
            None => h
                .session
                .refuse_picture(me, upload, element.id, Refusal::BadPicture, now),
        };
        self.host_effects(from, fx, now);
        self.drain_host(from, now);
    }

    /// What one of the host's windows drew after a picture of its, now that
    /// the picture is answered.
    fn drain_host(&mut self, from: Editor, now: std::time::Instant) {
        loop {
            let Some(next) = self
                .sessions
                .hosting
                .as_mut()
                .and_then(|h| h.gate_mut(from).next())
            else {
                return;
            };
            match next {
                Held::Draw(draw) => {
                    if let Err(e) = self.host_drew(from, draw.generation, draw.ops, now) {
                        tracing::debug!("the host's drawing held behind a picture: {e}");
                    }
                }
                Held::Picture(up) => {
                    self.check_host_picture(from, up, now);
                    return;
                }
            }
        }
    }

    /// Pictures and their answers that waited too long count as answered
    /// (the engine's tick).
    pub(super) fn pictures_tick(&mut self, now: std::time::Instant) {
        if let Some(w) = self.sessions.watching.as_mut()
            && w.gate.overdue(now)
        {
            w.gate.pending = None;
            self.drain_watch(now);
        }
        for from in [Editor::Overlay, Editor::Own] {
            if let Some(h) = self.sessions.hosting.as_mut()
                && h.gate_mut(from).overdue(now)
            {
                h.gate_mut(from).pending = None;
                self.drain_host(from, now);
            }
        }
    }
}

impl Actor {
    /// [`crate::Command::HostPicture`]: the host's picture from its board over
    /// the game, its bytes in base64.
    pub(in crate::rooms) fn overlay_picture(
        &mut self,
        generation: u32,
        element: &crate::board::BoardElement,
        data: &str,
        now: std::time::Instant,
    ) -> Result<Response, EngineError> {
        // Base64 of the largest picture, and no more.
        if data.len() > couch_proto::board::MAX_PICTURE_BYTES.div_ceil(3) * 4 {
            return Err(EngineError::Refused("that picture is too large"));
        }
        let bytes = data_encoding::BASE64
            .decode(data.as_bytes())
            .map_err(|_| EngineError::Refused("that picture isn't well formed"))?;
        let up = PictureUp {
            generation,
            element: element.into(),
            bytes,
        };
        self.host_picture(Editor::Overlay, up, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw() -> Held {
        Held::Draw(Draw {
            generation: 0,
            ops: Vec::new(),
        })
    }

    /// P17: a gate opens on its own upload's answer alone. An upload given up
    /// on (overdue, or its connection gone) may still be answered late, after
    /// a newer upload of the same element went: that answer lets nothing go.
    #[test]
    fn a_late_answer_never_opens_a_newer_uploads_gate() {
        let start = std::time::Instant::now();
        let mut gate = Gate::default();
        let first = gate.start(start);
        assert!(gate.overdue(start + ANSWER_WITHIN));
        gate.pending = None; // given up on, as the tick does
        let second = gate.start(start + ANSWER_WITHIN);
        assert_ne!(first, second);
        gate.hold(draw());
        assert!(!gate.answered(first), "the first's late answer");
        assert!(gate.next().is_none(), "the move still waits");
        assert!(gate.answered(second));
        assert!(gate.next().is_some());
        // A reset (a new connection), or another gate (a new watch), keeps
        // counting: an old answer is for a number no later upload has.
        let third = gate.start(start);
        gate.reset();
        let fourth = Gate::default().start(start);
        let fifth = gate.start(start);
        assert!(third != fourth && fourth != fifth);
        assert!(!gate.answered(third) && !gate.answered(fourth) && gate.answered(fifth));
    }
}
