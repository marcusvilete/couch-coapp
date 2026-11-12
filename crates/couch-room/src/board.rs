//! The board a session's host keeps (M27a, ADR 0013): the elements its
//! members drew, and the checks every batch goes through.
//!
//! Sans-IO like the rest of this crate: time comes in as `now`. The board
//! knows nothing of who watches or holds which seat; [`crate::session`]
//! checks that first, then hands the batch here.
//!
//! **Each member touches only its own.** Elements are keyed by their author,
//! which the caller takes from the connection, never from the batch, and a
//! number the author picked. An operation can only name the author's own
//! numbers, so nobody can replace, extend or remove another's element.
//!
//! **A batch is taken whole or not at all:** its changes are worked out
//! beside the board, checked against the limits, and only then applied.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

use couch_proto::{
    board::{
        Authored, BoardOp, Element, MAX_BATCHES_PER_SEC, MAX_BOARD_ELEMENTS,
        MAX_BOARD_PICTURE_BYTES, MAX_BOARD_PICTURES, MAX_BOARD_POINTS, MAX_MARKS_PER_SEC,
        MAX_MEMBER_ELEMENTS, MAX_MEMBER_PICTURES, MAX_PICTURE_BYTES, MAX_STROKE_POINTS, PictureId,
        Refusal, valid_batch,
    },
    ids::MemberId,
};
use sha2::{Digest, Sha256};

/// A picture's bytes as the board holds them (M27e): shared, never copied
/// per viewer.
pub type PictureBytes = Arc<[u8]>;

/// How long a fading element stays on the host's board once finished: a
/// little past its own stay and fade (wire revision 16: its author picks).
pub fn fade_drop(e: &Element) -> Duration {
    Duration::from_millis(couch_proto::board::fade_drop_ms(e.stay))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stored {
    element: Element,
    /// Its place in the drawing order: later ones are drawn over earlier
    /// ones. A replaced element keeps its place.
    seq: u64,
    /// When the host drops it: a finished fading element, a while after it
    /// arrived.
    drop_at: Option<Instant>,
}

/// A rate limit: `per_sec` a second, up to a second's worth at once.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn new(per_sec: u32, now: Instant) -> Self {
        Self {
            tokens: f64::from(per_sec),
            at: now,
        }
    }

    fn refill(&mut self, per_sec: u32, now: Instant) {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        let cap = f64::from(per_sec);
        self.tokens = (self.tokens + elapsed * cap).min(cap);
        self.at = now;
    }
}

#[derive(Debug, Clone, Copy)]
struct Rates {
    batches: Bucket,
    marks: Bucket,
}

/// What a batch the board took changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Taken {
    /// The number of elements changed (one added or removed): the console
    /// counts them.
    pub counts_changed: bool,
}

#[derive(Debug, Clone)]
pub struct Board {
    on: bool,
    /// Generations: a batch made before a clear of its member's elements
    /// (everyone's, or its own) that it hadn't heard of yet is dropped. A
    /// member's generation is the newer of everyone's and its own, all from
    /// one counter, so a clear of one member never stales another's.
    counter: u32,
    everyone: u32,
    own: BTreeMap<MemberId, u32>,
    stopped: BTreeSet<MemberId>,
    elements: BTreeMap<(MemberId, u32), Stored>,
    /// Each author's elements.
    per_member: BTreeMap<MemberId, usize>,
    points: usize,
    next_seq: u64,
    rates: BTreeMap<MemberId, Rates>,
    /// The host allows pictures (M27e).
    pictures_on: bool,
    /// The pictures' bytes, by id, with how many elements show each: a
    /// picture is kept exactly while an element shows it.
    pictures: BTreeMap<PictureId, (PictureBytes, usize)>,
    /// Their bytes, everyone's.
    picture_bytes: usize,
    /// Picture elements, by author.
    pictures_of: BTreeMap<MemberId, usize>,
}

impl Board {
    pub fn new(on: bool) -> Self {
        Self {
            on,
            counter: 0,
            everyone: 0,
            own: BTreeMap::new(),
            stopped: BTreeSet::new(),
            elements: BTreeMap::new(),
            per_member: BTreeMap::new(),
            points: 0,
            next_seq: 0,
            rates: BTreeMap::new(),
            pictures_on: false,
            pictures: BTreeMap::new(),
            picture_bytes: 0,
            pictures_of: BTreeMap::new(),
        }
    }

    // ---- Queries ----

    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Whether the host allows pictures (M27e).
    pub fn pictures_on(&self) -> bool {
        self.pictures_on
    }

    /// A picture's bytes, while an element on the board shows it.
    pub fn picture(&self, id: &PictureId) -> Option<&PictureBytes> {
        self.pictures.get(id).map(|(bytes, _)| bytes)
    }

    /// The pictures on the board, by id.
    pub fn picture_ids(&self) -> Vec<PictureId> {
        self.pictures.keys().copied().collect()
    }

    /// Pictures' bytes on the board, everyone's.
    pub fn picture_bytes(&self) -> usize {
        self.picture_bytes
    }

    /// The generation `member`'s batches must name.
    pub fn generation_of(&self, member: MemberId) -> u32 {
        self.own
            .get(&member)
            .copied()
            .unwrap_or(0)
            .max(self.everyone)
    }

    /// The generation everyone's last clear started.
    pub fn generation(&self) -> u32 {
        self.everyone
    }

    /// `member`'s elements, in drawing order.
    pub fn of(&self, member: MemberId) -> Vec<Element> {
        let mut own: Vec<&Stored> = self
            .elements
            .range((member, 0)..=(member, u32::MAX))
            .map(|(_, s)| s)
            .collect();
        own.sort_by_key(|s| s.seq);
        own.into_iter().map(|s| s.element.clone()).collect()
    }

    pub fn stopped(&self) -> Vec<MemberId> {
        self.stopped.iter().copied().collect()
    }

    pub fn is_stopped(&self, member: MemberId) -> bool {
        self.stopped.contains(&member)
    }

    /// Elements on the board.
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Points on the board, a text box's text a point a byte
    /// (`Element::load`).
    pub fn points(&self) -> usize {
        self.points
    }

    /// Each author with something on the board, and how many elements.
    pub fn counts(&self) -> Vec<(MemberId, usize)> {
        self.per_member.iter().map(|(m, n)| (*m, *n)).collect()
    }

    /// The board whole, in drawing order.
    pub fn snapshot(&self) -> Vec<Authored> {
        let mut all: Vec<(&(MemberId, u32), &Stored)> = self.elements.iter().collect();
        all.sort_by_key(|(_, s)| s.seq);
        all.into_iter()
            .map(|((author, _), s)| Authored {
                author: *author,
                element: s.element.clone(),
            })
            .collect()
    }

    /// One element, by its key.
    pub fn element(&self, author: MemberId, id: u32) -> Option<&Element> {
        self.elements.get(&(author, id)).map(|s| &s.element)
    }

    // ---- Changes ----

    /// Drawing on or off. Off clears the board; either way it is a fresh
    /// start for everyone. Returns whether it changed.
    pub fn set_on(&mut self, on: bool) -> bool {
        if self.on == on {
            return false;
        }
        self.on = on;
        if !on {
            self.clear();
        }
        true
    }

    /// Everything gone, in a new generation. Returns whether anything was
    /// there.
    pub fn clear(&mut self) -> bool {
        let any = !self.elements.is_empty();
        self.elements.clear();
        self.per_member.clear();
        self.points = 0;
        self.pictures.clear();
        self.picture_bytes = 0;
        self.pictures_of.clear();
        self.counter = self.counter.wrapping_add(1);
        self.everyone = self.counter;
        self.own.clear();
        any
    }

    /// A new generation for `member` alone (its elements cleared, or sent
    /// back to it): what it sent before it heard of it is dropped. Returns
    /// that generation.
    pub fn new_generation(&mut self, member: MemberId) -> u32 {
        self.counter = self.counter.wrapping_add(1);
        self.own.insert(member, self.counter);
        self.counter
    }

    /// One author's elements gone, in a new generation of its own. Returns
    /// whether it had any.
    pub fn clear_member(&mut self, member: MemberId) -> bool {
        self.new_generation(member);
        let keys: Vec<(MemberId, u32)> = self
            .elements
            .range((member, 0)..=(member, u32::MAX))
            .map(|(k, _)| *k)
            .collect();
        for key in &keys {
            self.remove(*key);
        }
        !keys.is_empty()
    }

    /// Stops `member` drawing, clearing its elements, or lets it draw again.
    /// Returns whether it changed.
    pub fn set_stopped(&mut self, member: MemberId, stopped: bool) -> bool {
        let changed = if stopped {
            self.stopped.insert(member)
        } else {
            self.stopped.remove(&member)
        };
        if stopped && changed {
            self.clear_member(member);
        }
        changed
    }

    /// Forgets whom the host stopped but `keep` (members gone from the
    /// room), so the list stays within a room's size.
    pub fn forget_stopped(&mut self, keep: impl Fn(MemberId) -> bool) {
        self.stopped.retain(|m| keep(*m));
    }

    /// Finishes `member`'s strokes still being drawn and text boxes still
    /// being typed (it can no longer finish them: refused, or gone), so
    /// fading ones fade. Returns the operations that did it, for the others
    /// to hear: an empty append for a stroke, the box again for a text box
    /// (an append is a stroke's alone).
    pub fn finish_open(&mut self, member: MemberId, now: Instant) -> Vec<BoardOp> {
        let mut ops = Vec::new();
        for ((_, id), s) in self.elements.range_mut((member, 0)..=(member, u32::MAX)) {
            if !s.element.done {
                s.element.done = true;
                if s.element.fades {
                    s.drop_at = Some(now + fade_drop(&s.element));
                }
                ops.push(if s.element.kind.is_stroke() {
                    BoardOp::Append {
                        id: *id,
                        points: Vec::new(),
                        done: true,
                    }
                } else {
                    BoardOp::Put(s.element.clone())
                });
            }
        }
        ops
    }

    /// A member is gone from the session: its rate limits go with it. What
    /// it drew stays until the host clears it.
    pub fn forget_rates(&mut self, member: MemberId) {
        self.rates.remove(&member);
    }

    /// Drops the finished fading elements whose time is up. Returns whether
    /// any went.
    pub fn expire(&mut self, now: Instant) -> bool {
        let due: Vec<(MemberId, u32)> = self
            .elements
            .iter()
            .filter(|(_, s)| s.drop_at.is_some_and(|t| t <= now))
            .map(|(k, _)| *k)
            .collect();
        for key in &due {
            self.remove(*key);
        }
        !due.is_empty()
    }

    /// When the next fading element is due to go, if any.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.elements.values().filter_map(|s| s.drop_at).min()
    }

    fn remove(&mut self, key: (MemberId, u32)) {
        if let Some(old) = self.elements.remove(&key) {
            self.points -= old.element.load();
            if let Some(id) = old.element.picture {
                self.unshow(key.0, id);
            }
            if let Some(n) = self.per_member.get_mut(&key.0) {
                *n -= 1;
                if *n == 0 {
                    self.per_member.remove(&key.0);
                }
            }
        }
    }

    /// `author`'s batch, made in `generation`, taken whole or refused.
    /// `blocked` is why the session refuses the author (the keyboard seat,
    /// nothing streamed), the caller having checked it watches. In order:
    /// the rates (every batch counts, refused or not: refusals must not be a
    /// way to try faster), the generation (`Stale`: dropped, nothing to say,
    /// as the member hears of the clear), then `blocked`, drawing on, the
    /// author not stopped, the batch well formed, touching only the
    /// author's own elements, and the limits.
    ///
    /// **A picture comes into being only by [`Self::put_picture`]:** a
    /// `Put` of one here only moves or resizes a picture element of the
    /// author's already there with the same picture.
    pub fn apply(
        &mut self,
        author: MemberId,
        generation: u32,
        ops: &[BoardOp],
        now: Instant,
        blocked: Option<Refusal>,
    ) -> Result<Taken, Refusal> {
        self.take(author, generation, ops, now, blocked, None)
    }

    /// `author`'s picture, made in `generation` (M27e): `element` (a picture
    /// element, new or replacing one of its own) with `bytes`, which the
    /// caller has checked decode as a picture within the limits
    /// (`couch_media::picture::check`). Taken as a batch of that one `Put`,
    /// under the same checks as [`Self::apply`] and these: pictures on, the
    /// bytes what the element names and within their limit, and room for
    /// them ([`MAX_MEMBER_PICTURES`], [`MAX_BOARD_PICTURES`],
    /// [`MAX_BOARD_PICTURE_BYTES`]).
    pub fn put_picture(
        &mut self,
        author: MemberId,
        generation: u32,
        element: Element,
        bytes: &PictureBytes,
        now: Instant,
        blocked: Option<Refusal>,
    ) -> Result<Taken, Refusal> {
        let ops = [BoardOp::Put(element)];
        self.take(author, generation, &ops, now, blocked, Some(bytes))
    }

    /// Whether `author`'s picture `element`, made in `generation`, could be
    /// taken as far as the cheap checks go (P17): asked before its bytes are
    /// read, so a member refused (too fast, stale, drawing off or blocked,
    /// stopped, pictures off) costs the host no read and no decode. Refused,
    /// it counts toward the rates as any refused batch does (refusals are no
    /// way to try faster); let through, it spends nothing yet, since
    /// [`Self::put_picture`] counts it then, once.
    pub fn picture_coming(
        &mut self,
        author: MemberId,
        generation: u32,
        element: &Element,
        now: Instant,
        blocked: Option<Refusal>,
    ) -> Result<(), Refusal> {
        let ops = [BoardOp::Put(element.clone())];
        self.admit(author, generation, &ops, now, blocked, true, false)
    }

    /// A batch's checks before the board is looked at: the rates first, then
    /// the generation, `blocked`, drawing on, the author not stopped, the
    /// batch well formed, and for a `picture` pictures on. The batch counts
    /// toward the rates whatever the answer, but for a picture's header let
    /// through (`spend` false), which its upload counts later.
    #[allow(clippy::too_many_arguments)]
    fn admit(
        &mut self,
        author: MemberId,
        generation: u32,
        ops: &[BoardOp],
        now: Instant,
        blocked: Option<Refusal>,
        picture: bool,
        spend: bool,
    ) -> Result<(), Refusal> {
        let rates = self.rates.entry(author).or_insert_with(|| Rates {
            batches: Bucket::new(MAX_BATCHES_PER_SEC, now),
            marks: Bucket::new(MAX_MARKS_PER_SEC, now),
        });
        rates.batches.refill(MAX_BATCHES_PER_SEC, now);
        rates.marks.refill(MAX_MARKS_PER_SEC, now);
        let marks = ops.iter().map(BoardOp::marks).sum::<usize>() as f64;
        if rates.batches.tokens < 1.0 || rates.marks.tokens < marks {
            // Spent anyway: a member over its rate stays over it.
            rates.batches.tokens = (rates.batches.tokens - 1.0).max(0.0);
            return Err(Refusal::TooFast);
        }
        let checked = self.checks(author, generation, ops, blocked, picture);
        if spend || checked.is_err() {
            let rates = self.rates.get_mut(&author).expect("made above");
            rates.batches.tokens -= 1.0;
            rates.marks.tokens -= marks;
        }
        checked
    }

    /// A batch refused before the board saw it, counted toward the rates as
    /// any refused batch is: a picture whose bytes weren't one (P17's review).
    pub fn count_refused(&mut self, author: MemberId, now: Instant) {
        let rates = self.rates.entry(author).or_insert_with(|| Rates {
            batches: Bucket::new(MAX_BATCHES_PER_SEC, now),
            marks: Bucket::new(MAX_MARKS_PER_SEC, now),
        });
        rates.batches.refill(MAX_BATCHES_PER_SEC, now);
        rates.batches.tokens = (rates.batches.tokens - 1.0).max(0.0);
    }

    /// [`Self::admit`]'s checks after the rates, in their order.
    fn checks(
        &self,
        author: MemberId,
        generation: u32,
        ops: &[BoardOp],
        blocked: Option<Refusal>,
        picture: bool,
    ) -> Result<(), Refusal> {
        if generation != self.generation_of(author) {
            return Err(Refusal::Stale);
        }
        if let Some(why) = blocked {
            return Err(why);
        }
        if !self.on {
            return Err(Refusal::Off);
        }
        if self.stopped.contains(&author) {
            return Err(Refusal::Stopped);
        }
        if !valid_batch(ops) {
            return Err(Refusal::Invalid);
        }
        if picture && !self.pictures_on {
            return Err(Refusal::PicturesOff);
        }
        Ok(())
    }

    fn take(
        &mut self,
        author: MemberId,
        generation: u32,
        ops: &[BoardOp],
        now: Instant,
        blocked: Option<Refusal>,
        upload: Option<&PictureBytes>,
    ) -> Result<Taken, Refusal> {
        self.admit(
            author,
            generation,
            ops,
            now,
            blocked,
            upload.is_some(),
            true,
        )?;
        if let Some(bytes) = upload {
            let named = match ops {
                [BoardOp::Put(e)] => e.picture,
                _ => None,
            };
            if bytes.is_empty()
                || bytes.len() > MAX_PICTURE_BYTES
                || named != Some(PictureId(Sha256::digest(bytes).into()))
            {
                return Err(Refusal::BadPicture);
            }
        }

        // The author's elements as the batch leaves them, beside the board.
        let mut changes: BTreeMap<u32, Option<Stored>> = BTreeMap::new();
        let mut seq = self.next_seq;
        let drop_at = |e: &Element| (e.done && e.fades).then(|| now + fade_drop(e));
        for op in ops {
            let current = |changes: &BTreeMap<u32, Option<Stored>>, id: u32| match changes.get(&id)
            {
                Some(change) => change.clone(),
                None => self.elements.get(&(author, id)).cloned(),
            };
            match op {
                BoardOp::Put(element) => {
                    let current = current(&changes, element.id);
                    // A picture only by its upload, or moved: the same
                    // picture already there as this element.
                    if element.picture.is_some()
                        && upload.is_none()
                        && current
                            .as_ref()
                            .is_none_or(|old| old.element.picture != element.picture)
                    {
                        return Err(Refusal::Invalid);
                    }
                    let place = match current {
                        Some(old) => old.seq,
                        None => {
                            seq += 1;
                            seq
                        }
                    };
                    changes.insert(
                        element.id,
                        Some(Stored {
                            element: element.clone(),
                            seq: place,
                            drop_at: drop_at(element),
                        }),
                    );
                }
                BoardOp::Append { id, points, done } => {
                    let Some(mut stored) = current(&changes, *id) else {
                        return Err(Refusal::Invalid);
                    };
                    let e = &mut stored.element;
                    if !e.kind.is_stroke()
                        || e.done
                        || e.points.len() + points.len() > MAX_STROKE_POINTS
                    {
                        return Err(Refusal::Invalid);
                    }
                    e.points.extend_from_slice(points);
                    e.done = *done;
                    stored.drop_at = drop_at(&stored.element);
                    changes.insert(*id, Some(stored));
                }
                // Ids it doesn't have (cleared by the host meanwhile, say)
                // are nothing to remove.
                BoardOp::Remove(ids) => {
                    for id in ids {
                        changes.insert(*id, None);
                    }
                }
                BoardOp::Laser { .. } | BoardOp::Ping(_) => {}
            }
        }

        // The counts the batch would leave.
        let mut mine = self.per_member.get(&author).copied().unwrap_or(0) as isize;
        let mut total = self.elements.len() as isize;
        let mut points = self.points as isize;
        for (id, change) in &changes {
            let old = self.elements.get(&(author, *id));
            let had = isize::from(old.is_some());
            let has = isize::from(change.is_some());
            mine += has - had;
            total += has - had;
            points += change.as_ref().map_or(0, |s| s.element.load() as isize)
                - old.map_or(0, |s| s.element.load() as isize);
        }
        let grew = |after: isize, before: usize| after > before as isize;
        let before_mine = self.per_member.get(&author).copied().unwrap_or(0);
        if grew(mine, before_mine) && mine > MAX_MEMBER_ELEMENTS as isize {
            return Err(Refusal::TooMany);
        }
        if (grew(total, self.elements.len()) && total > MAX_BOARD_ELEMENTS as isize)
            || (grew(points, self.points) && points > MAX_BOARD_POINTS as isize)
        {
            return Err(Refusal::BoardFull);
        }
        // Pictures: the author's, the board's, and their bytes. Only an
        // upload adds one, so only an upload can be refused for room.
        if let (Some(bytes), [BoardOp::Put(e)]) = (upload, ops) {
            let replaced = self
                .elements
                .get(&(author, e.id))
                .is_some_and(|old| old.element.picture.is_some());
            let added = usize::from(!replaced);
            let new_bytes = match e.picture {
                Some(id) if !self.pictures.contains_key(&id) => bytes.len(),
                _ => 0,
            };
            let mine = self.pictures_of.get(&author).copied().unwrap_or(0);
            let all: usize = self.pictures_of.values().sum();
            if mine + added > MAX_MEMBER_PICTURES
                || all + added > MAX_BOARD_PICTURES
                || self.picture_bytes + new_bytes > MAX_BOARD_PICTURE_BYTES
            {
                return Err(Refusal::TooManyPictures);
            }
        }

        // Taken: apply it.
        let counts_changed = mine != before_mine as isize;
        self.next_seq = seq;
        for (id, change) in changes {
            // A picture shown again before the element showing it goes, so a
            // picture moved is never dropped between the two.
            if let Some(picture) = change.as_ref().and_then(|s| s.element.picture) {
                self.show(author, picture, upload);
            }
            self.remove((author, id));
            if let Some(stored) = change {
                self.points += stored.element.load();
                *self.per_member.entry(author).or_default() += 1;
                self.elements.insert((author, id), stored);
            }
        }
        Ok(Taken { counts_changed })
    }

    /// An element shows `id`, `author`'s: held while one does. A picture
    /// new to the board comes with its bytes.
    fn show(&mut self, author: MemberId, id: PictureId, bytes: Option<&PictureBytes>) {
        match self.pictures.get_mut(&id) {
            Some((_, n)) => *n += 1,
            None => {
                let bytes = bytes.expect("a new picture comes with its bytes").clone();
                self.picture_bytes += bytes.len();
                self.pictures.insert(id, (bytes, 1));
            }
        }
        *self.pictures_of.entry(author).or_default() += 1;
    }

    /// An element of `author`'s showing `id` went: the picture goes with the
    /// last one.
    fn unshow(&mut self, author: MemberId, id: PictureId) {
        if let Some((bytes, n)) = self.pictures.get_mut(&id) {
            *n -= 1;
            if *n == 0 {
                self.picture_bytes -= bytes.len();
                self.pictures.remove(&id);
            }
        }
        if let Some(n) = self.pictures_of.get_mut(&author) {
            *n -= 1;
            if *n == 0 {
                self.pictures_of.remove(&author);
            }
        }
    }

    /// Pictures allowed or not (M27e). Off removes every picture element
    /// from the board; returns them by author, for windows to hear, and
    /// whether the setting changed.
    pub fn set_pictures(&mut self, on: bool) -> (bool, Vec<(MemberId, Vec<u32>)>) {
        if self.pictures_on == on {
            return (false, Vec::new());
        }
        self.pictures_on = on;
        if on {
            return (true, Vec::new());
        }
        let keys: Vec<(MemberId, u32)> = self
            .elements
            .iter()
            .filter(|(_, s)| s.element.picture.is_some())
            .map(|(k, _)| *k)
            .collect();
        let mut removed: BTreeMap<MemberId, Vec<u32>> = BTreeMap::new();
        for key in keys {
            self.remove(key);
            removed.entry(key.0).or_default().push(key.1);
        }
        (true, removed.into_iter().collect())
    }
}

/// The board as a viewer keeps it: the host's updates, its own operations
/// and the host's clears, in the order heard. The watch pipeline keeps one
/// for a stream window started again, and the tests check it against the
/// host's.
#[derive(Debug, Clone, Default)]
pub struct Replica {
    generation: u32,
    /// Each element with its place in the drawing order.
    elements: BTreeMap<(MemberId, u32), (u64, Element)>,
    next: u64,
}

impl Replica {
    /// The generation the member's next batches name.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// `author`'s operations: the host's update, or our own as sent. An
    /// append to an element it doesn't have is nothing (the host's resync
    /// follows any batch it refused).
    pub fn heard(&mut self, author: MemberId, ops: &[BoardOp]) {
        for op in ops {
            match op {
                BoardOp::Put(e) => {
                    let place = match self.elements.get(&(author, e.id)) {
                        Some((place, _)) => *place,
                        None => {
                            self.next += 1;
                            self.next
                        }
                    };
                    self.elements.insert((author, e.id), (place, e.clone()));
                }
                BoardOp::Append { id, points, done } => {
                    if let Some((_, e)) = self.elements.get_mut(&(author, *id))
                        && e.points.len() + points.len() <= MAX_STROKE_POINTS
                    {
                        e.points.extend_from_slice(points);
                        e.done = *done;
                    }
                }
                BoardOp::Remove(ids) => {
                    for id in ids {
                        self.elements.remove(&(author, *id));
                    }
                }
                BoardOp::Laser { .. } | BoardOp::Ping(_) => {}
            }
        }
    }

    /// A part of the host's snapshot: the first replaces everything.
    pub fn snapshot_part(&mut self, generation: u32, first: bool, elements: &[Authored]) {
        if first {
            self.elements.clear();
        }
        self.generation = generation;
        for a in elements {
            self.heard(a.author, &[BoardOp::Put(a.element.clone())]);
        }
    }

    /// The host cleared one author's elements, or everyone's.
    /// `me`, the member whose copy this is, takes the new generation only
    /// from a clear of its own elements or everyone's: another member's has
    /// a generation of its own.
    pub fn cleared(&mut self, generation: u32, author: Option<MemberId>, me: Option<MemberId>) {
        if author.is_none() || author == me {
            self.generation = generation;
        }
        match author {
            Some(author) => self.elements.retain(|(a, _), _| *a != author),
            None => self.elements.clear(),
        }
    }

    /// Keeps only the elements `keep` says.
    pub fn retain(&mut self, mut keep: impl FnMut(MemberId, &Element) -> bool) {
        self.elements.retain(|(a, _), (_, e)| keep(*a, e));
    }

    /// The board in drawing order.
    pub fn snapshot(&self) -> Vec<Authored> {
        let mut all: Vec<&(MemberId, u32)> = self.elements.keys().collect();
        all.sort_by_key(|k| self.elements[k].0);
        all.into_iter()
            .map(|k| Authored {
                author: k.0,
                element: self.elements[k].1.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use couch_proto::{
        board::{BoardSnapshot, ElementKind, MAX_BATCH_OPS, MAX_TEXT_CHARS, Point},
        ids::SessionId,
    };
    use proptest::prelude::*;

    use super::*;

    /// When the host drops a finished fading element of the default stay.
    const FADE_DROP: Duration = Duration::from_millis(7_000);

    const BIA: MemberId = MemberId([1; 32]);
    const CAI: MemberId = MemberId([2; 32]);

    impl Board {
        /// A batch made in the current generation.
        fn go(
            &mut self,
            author: MemberId,
            ops: &[BoardOp],
            now: Instant,
        ) -> Result<Taken, Refusal> {
            self.apply(author, self.generation_of(author), ops, now, None)
        }
    }

    #[test]
    fn a_batch_from_before_a_clear_is_refused() {
        let now = Instant::now();
        let mut board = Board::new(true);
        let before = board.generation_of(BIA);
        board.go(BIA, &[BoardOp::Put(rect(1))], now).unwrap();
        assert!(board.clear());
        assert_ne!(board.generation_of(BIA), before);
        assert_eq!(
            board.apply(BIA, before, &[BoardOp::Put(rect(2))], now, None),
            Err(Refusal::Stale)
        );
        assert!(board.is_empty());
        // Clearing one member starts a new generation for it alone: the
        // others' batches on their way still count (the review's finding).
        let (bia, cai) = (board.generation_of(BIA), board.generation_of(CAI));
        board.clear_member(CAI);
        assert_eq!(board.generation_of(BIA), bia);
        assert_ne!(board.generation_of(CAI), cai);
        board
            .apply(BIA, bia, &[BoardOp::Put(rect(2))], now, None)
            .unwrap();
        assert_eq!(
            board.apply(CAI, cai, &[BoardOp::Put(rect(2))], now, None),
            Err(Refusal::Stale)
        );
        // A clear of everyone after it is newer than its own.
        board.clear();
        assert_eq!(board.generation_of(CAI), board.generation_of(BIA));
        assert_eq!(board.generation_of(CAI), board.generation());
    }

    #[test]
    fn every_batch_counts_toward_the_rates_refused_or_not() {
        let now = Instant::now();
        let mut board = Board::new(true);
        let stale = board.generation_of(BIA).wrapping_sub(1);
        for _ in 0..MAX_BATCHES_PER_SEC {
            assert_eq!(
                board.apply(BIA, stale, &[BoardOp::Ping(Point::new(1, 1))], now, None),
                Err(Refusal::Stale)
            );
        }
        assert_eq!(
            board.go(BIA, &[BoardOp::Ping(Point::new(1, 1))], now),
            Err(Refusal::TooFast)
        );
    }

    #[test]
    fn open_strokes_are_finished_for_a_member_who_cant() {
        let now = Instant::now();
        let mut board = Board::new(true);
        board
            .go(BIA, &[BoardOp::Put(pen(1, 2, false, true))], now)
            .unwrap();
        board
            .go(BIA, &[BoardOp::Put(pen(2, 2, true, false))], now)
            .unwrap();
        let ops = board.finish_open(BIA, now);
        assert_eq!(
            ops,
            [BoardOp::Append {
                id: 1,
                points: vec![],
                done: true
            }]
        );
        assert!(board.element(BIA, 1).unwrap().done);
        assert_eq!(board.next_expiry(), Some(now + FADE_DROP), "it fades now");
        assert!(board.finish_open(BIA, now).is_empty());
        assert_eq!(board.of(BIA).len(), 2);
    }

    fn pen(id: u32, points: usize, done: bool, fades: bool) -> Element {
        Element {
            picture: None,
            id,
            kind: ElementKind::Pen,
            colour: 0,
            width: 1,
            fades,
            stay: 4,
            done,
            points: (0..points).map(|i| Point::new(i as u16, 7)).collect(),
            text: String::new(),
        }
    }

    fn rect(id: u32) -> Element {
        Element {
            picture: None,
            id,
            kind: ElementKind::Rectangle,
            colour: 3,
            width: 0,
            fades: false,
            stay: 4,
            done: true,
            points: vec![Point::new(1, 1), Point::new(9, 9)],
            text: String::new(),
        }
    }

    #[test]
    fn nobody_touches_anothers_element() {
        let now = Instant::now();
        let mut board = Board::new(true);
        board
            .go(BIA, &[BoardOp::Put(pen(1, 3, false, false))], now)
            .unwrap();
        // Cai's id 1 is Cai's own: a new element beside Bia's.
        board.go(CAI, &[BoardOp::Put(rect(1))], now).unwrap();
        assert_eq!(board.len(), 2);
        assert_eq!(board.element(BIA, 1).unwrap().kind, ElementKind::Pen);
        // Cai can't extend or remove Bia's.
        assert_eq!(
            board.go(
                CAI,
                &[BoardOp::Append {
                    id: 9,
                    points: vec![Point::new(0, 0)],
                    done: true
                }],
                now
            ),
            Err(Refusal::Invalid)
        );
        board.go(CAI, &[BoardOp::Remove(vec![1, 2])], now).unwrap();
        assert!(board.element(BIA, 1).is_some());
        assert!(board.element(CAI, 1).is_none());
    }

    #[test]
    fn a_stroke_grows_until_finished() {
        let now = Instant::now();
        let mut board = Board::new(true);
        board
            .go(BIA, &[BoardOp::Put(pen(1, 2, false, false))], now)
            .unwrap();
        let more = |done| BoardOp::Append {
            id: 1,
            points: vec![Point::new(5, 5); 3],
            done,
        };
        assert_eq!(
            board.go(BIA, &[more(false)], now),
            Ok(Taken {
                counts_changed: false
            })
        );
        board.go(BIA, &[more(true)], now).unwrap();
        assert_eq!(board.element(BIA, 1).unwrap().points.len(), 8);
        assert_eq!(board.points(), 8);
        assert_eq!(
            board.go(BIA, &[more(true)], now),
            Err(Refusal::Invalid),
            "finished"
        );
        // A shape never grows.
        board.go(BIA, &[BoardOp::Put(rect(2))], now).unwrap();
        let grow_rect = BoardOp::Append {
            id: 2,
            points: vec![Point::new(0, 0)],
            done: true,
        };
        assert_eq!(board.go(BIA, &[grow_rect], now), Err(Refusal::Invalid));
        // Nor past its limit.
        board
            .go(
                BIA,
                &[BoardOp::Put(pen(3, MAX_STROKE_POINTS - 1, false, false))],
                now,
            )
            .unwrap();
        let too_long = BoardOp::Append {
            id: 3,
            points: vec![Point::new(0, 0); 2],
            done: true,
        };
        assert_eq!(board.go(BIA, &[too_long], now), Err(Refusal::Invalid));
    }

    #[test]
    fn a_batch_is_taken_whole_or_not_at_all() {
        let now = Instant::now();
        let mut board = Board::new(true);
        let bad = [
            BoardOp::Put(rect(1)),
            BoardOp::Append {
                id: 1,
                points: vec![Point::new(0, 0)],
                done: true,
            },
        ];
        assert_eq!(board.go(BIA, &bad, now), Err(Refusal::Invalid));
        assert!(board.is_empty());
    }

    #[test]
    fn off_and_stopped_change_nothing() {
        let now = Instant::now();
        let mut board = Board::new(false);
        assert_eq!(
            board.go(BIA, &[BoardOp::Put(rect(1))], now),
            Err(Refusal::Off)
        );
        assert!(board.set_on(true));
        board.go(BIA, &[BoardOp::Put(rect(1))], now).unwrap();
        board.go(CAI, &[BoardOp::Put(rect(1))], now).unwrap();
        assert!(board.set_stopped(BIA, true));
        assert!(board.element(BIA, 1).is_none(), "stopping clears theirs");
        assert!(board.element(CAI, 1).is_some());
        assert_eq!(
            board.go(BIA, &[BoardOp::Ping(Point::new(0, 0))], now),
            Err(Refusal::Stopped)
        );
        assert!(board.set_stopped(BIA, false));
        board.go(BIA, &[BoardOp::Put(rect(2))], now).unwrap();
        assert!(board.set_on(false));
        assert!(board.is_empty(), "off clears the board");
    }

    #[test]
    fn the_limits_hold() {
        let now = Instant::now();
        let mut board = Board::new(true);
        let mut later = now;
        let mut put = |board: &mut Board, who, first: u32, n: u32| {
            later += Duration::from_secs(1);
            let ops: Vec<BoardOp> = (first..first + n).map(|i| BoardOp::Put(rect(i))).collect();
            board.go(who, &ops, later)
        };
        for k in 0..(MAX_MEMBER_ELEMENTS / MAX_BATCH_OPS) as u32 {
            put(&mut board, BIA, k * 64, 64).unwrap();
        }
        let have = board.len() as u32;
        put(&mut board, BIA, have, (MAX_MEMBER_ELEMENTS as u32) - have).unwrap();
        assert_eq!(
            put(&mut board, BIA, 10_000, 1),
            Err(Refusal::TooMany),
            "{}",
            board.len()
        );
        // Removing while full is fine, and so is replacing.
        later += Duration::from_secs(1);
        board
            .go(
                BIA,
                &[BoardOp::Put(rect(0)), BoardOp::Remove(vec![1])],
                later,
            )
            .unwrap();
        assert_eq!(board.len(), MAX_MEMBER_ELEMENTS - 1);
    }

    #[test]
    fn the_board_fills() {
        let now = Instant::now();
        let mut board = Board::new(true);
        let mut later = now;
        let mut taken = 0;
        'members: for m in 0..10u8 {
            for k in 0..5u32 {
                later += Duration::from_secs(1);
                let ops: Vec<BoardOp> = (0..60).map(|i| BoardOp::Put(rect(k * 64 + i))).collect();
                match board.go(MemberId([m; 32]), &ops, later) {
                    Ok(_) => taken += 60,
                    Err(Refusal::BoardFull) => break 'members,
                    Err(e) => panic!("{e:?}"),
                }
            }
        }
        assert_eq!(taken, MAX_BOARD_ELEMENTS);
        assert_eq!(board.len(), MAX_BOARD_ELEMENTS);
    }

    #[test]
    fn too_fast_is_refused() {
        let now = Instant::now();
        let mut board = Board::new(true);
        let ping = [BoardOp::Ping(Point::new(1, 1))];
        for _ in 0..MAX_BATCHES_PER_SEC {
            board.go(BIA, &ping, now).unwrap();
        }
        assert_eq!(board.go(BIA, &ping, now), Err(Refusal::TooFast));
        // Another member has its own.
        board.go(CAI, &ping, now).unwrap();
        // A second later, it may again.
        board.go(BIA, &ping, now + Duration::from_secs(1)).unwrap();
        // Laser points count as marks.
        let laser = BoardOp::Laser {
            points: vec![Point::new(0, 0); 61],
            start: true,
            colour: 0,
        };
        assert_eq!(
            board.go(CAI, &[laser], now + Duration::from_secs(5)),
            Err(Refusal::TooFast)
        );
    }

    #[test]
    fn fading_elements_leave_the_board() {
        let now = Instant::now();
        let mut board = Board::new(true);
        board
            .go(BIA, &[BoardOp::Put(pen(1, 2, false, true))], now)
            .unwrap();
        board
            .go(BIA, &[BoardOp::Put(pen(2, 2, true, false))], now)
            .unwrap();
        assert_eq!(board.next_expiry(), None, "only once finished");
        let done = now + Duration::from_secs(1);
        board
            .go(
                BIA,
                &[BoardOp::Append {
                    id: 1,
                    points: vec![],
                    done: true,
                }],
                done,
            )
            .unwrap();
        assert_eq!(board.next_expiry(), Some(done + FADE_DROP));
        assert!(!board.expire(done + FADE_DROP - Duration::from_millis(1)));
        assert!(board.expire(done + FADE_DROP));
        assert!(board.element(BIA, 1).is_none());
        assert!(board.element(BIA, 2).is_some(), "it stays");
    }

    #[test]
    fn a_longer_stay_is_kept_longer() {
        let now = Instant::now();
        let mut board = Board::new(true);
        let long = Element {
            stay: 15,
            ..pen(1, 2, true, true)
        };
        board.go(BIA, &[BoardOp::Put(long)], now).unwrap();
        assert_eq!(
            fade_drop(board.element(BIA, 1).unwrap()),
            Duration::from_secs(18)
        );
        assert!(!board.expire(now + FADE_DROP));
        assert!(board.expire(now + Duration::from_secs(18)));
        // A stay that isn't offered is refused.
        let odd = Element {
            stay: 5,
            ..pen(2, 2, true, true)
        };
        assert!(board.go(BIA, &[BoardOp::Put(odd)], now).is_err());
    }

    fn text(id: u32, text: &str, done: bool) -> Element {
        Element {
            kind: ElementKind::Text,
            done,
            text: text.into(),
            ..rect(id)
        }
    }

    #[test]
    fn a_text_box_is_typed_then_finished() {
        let now = Instant::now();
        let mut board = Board::new(true);
        let typing = |t| {
            BoardOp::Put(Element {
                fades: true,
                ..text(1, t, false)
            })
        };
        board.go(BIA, &[typing("H")], now).unwrap();
        board.go(BIA, &[typing("Hi\nthere")], now).unwrap();
        assert_eq!(board.points(), 2 + 8, "its text weighs a point a byte");
        assert_eq!(board.next_expiry(), None, "no fading while typed");
        // Points are a stroke's alone, even to finish it.
        assert_eq!(
            board.go(
                BIA,
                &[BoardOp::Append {
                    id: 1,
                    points: vec![],
                    done: true
                }],
                now
            ),
            Err(Refusal::Invalid)
        );
        // Nor may it lose its text, nor carry what a box can't.
        for bad in ["", "  ", "tab\there"] {
            assert_eq!(
                board.go(BIA, &[BoardOp::Put(text(1, bad, false))], now),
                Err(Refusal::Invalid),
                "{bad:?}"
            );
        }
        // Its author gone mid-word: the host finishes it as a box again.
        let ops = board.finish_open(BIA, now);
        assert_eq!(
            ops,
            [BoardOp::Put(Element {
                fades: true,
                ..text(1, "Hi\nthere", true)
            })]
        );
        assert_eq!(board.next_expiry(), Some(now + FADE_DROP));
        // Text counts against the board's points: long boxes fill it.
        let long: String = "😀".repeat(MAX_TEXT_CHARS);
        let mut later = now;
        let mut taken = 0;
        'members: for m in 3..250u8 {
            for id in 0..5 {
                later += Duration::from_secs(1);
                match board.go(
                    MemberId([m; 32]),
                    &[BoardOp::Put(text(id, &long, true))],
                    later,
                ) {
                    Ok(_) => taken += 1,
                    Err(Refusal::BoardFull) => break 'members,
                    Err(e) => panic!("{e:?}"),
                }
            }
        }
        assert_eq!(taken, (MAX_BOARD_POINTS - 10) / 2_002);
        assert!(board.points() <= MAX_BOARD_POINTS);
    }

    #[test]
    fn a_snapshot_keeps_the_drawing_order() {
        let now = Instant::now();
        let mut board = Board::new(true);
        board.go(CAI, &[BoardOp::Put(rect(9))], now).unwrap();
        board.go(BIA, &[BoardOp::Put(rect(1))], now).unwrap();
        // Replaced, it keeps its place.
        board.go(CAI, &[BoardOp::Put(rect(9))], now).unwrap();
        let order: Vec<(MemberId, u32)> = board
            .snapshot()
            .iter()
            .map(|a| (a.author, a.element.id))
            .collect();
        assert_eq!(order, [(CAI, 9), (BIA, 1)]);
    }

    #[test]
    fn a_picture_comes_only_by_its_upload_and_is_kept_while_shown() {
        let now = Instant::now();
        let mut board = Board::new(true);
        let bytes = test_picture(1);
        let id = picture_of(&bytes);
        let generation = board.generation_of(BIA);
        assert_eq!(
            board.put_picture(BIA, generation, picture_element(1, id), &bytes, now, None),
            Err(Refusal::PicturesOff),
            "off by default"
        );
        board.set_pictures(true);
        assert_eq!(
            board.go(BIA, &[BoardOp::Put(picture_element(1, id))], now),
            Err(Refusal::Invalid),
            "never by a batch"
        );
        assert_eq!(
            board.put_picture(
                BIA,
                generation,
                picture_element(1, picture_of(&test_picture(2))),
                &bytes,
                now,
                None
            ),
            Err(Refusal::BadPicture),
            "the bytes must be what it names"
        );
        board
            .put_picture(BIA, generation, picture_element(1, id), &bytes, now, None)
            .unwrap();
        assert_eq!(board.picture(&id), Some(&bytes));
        assert_eq!(board.picture_bytes(), bytes.len());

        // Moved by a batch: the same picture, kept.
        let mut moved = picture_element(1, id);
        moved.points = vec![Point::new(5, 5), Point::new(600, 400)];
        board.go(BIA, &[BoardOp::Put(moved.clone())], now).unwrap();
        assert_eq!(board.element(BIA, 1), Some(&moved));
        assert_eq!(board.picture(&id), Some(&bytes));
        // Another picture by a batch, or another's: refused.
        let other = picture_of(&test_picture(3));
        assert_eq!(
            board.go(BIA, &[BoardOp::Put(picture_element(1, other))], now),
            Err(Refusal::Invalid)
        );
        assert_eq!(
            board.go(CAI, &[BoardOp::Put(picture_element(1, id))], now),
            Err(Refusal::Invalid),
            "another's picture isn't Cai's to show"
        );
        // Shown twice (Cai uploads the same picture), it stays until both go.
        let generation_cai = board.generation_of(CAI);
        board
            .put_picture(
                CAI,
                generation_cai,
                picture_element(7, id),
                &bytes,
                now,
                None,
            )
            .unwrap();
        assert_eq!(board.picture_bytes(), bytes.len(), "held once");
        board.go(BIA, &[BoardOp::Remove(vec![1])], now).unwrap();
        assert_eq!(board.picture(&id), Some(&bytes));
        board.go(CAI, &[BoardOp::Remove(vec![7])], now).unwrap();
        assert_eq!(board.picture(&id), None);
        assert_eq!(board.picture_bytes(), 0);

        // Fading, it goes when its time is up, and its bytes with it.
        let fading = Element {
            fades: true,
            ..picture_element(2, id)
        };
        board
            .put_picture(BIA, generation, fading, &bytes, now, None)
            .unwrap();
        board.expire(now + FADE_DROP);
        assert!(board.is_empty() && board.picture_ids().is_empty());
    }

    #[test]
    fn pictures_have_limits_of_their_own() {
        let now = Instant::now();
        let mut board = Board::new(true);
        board.set_pictures(true);
        let mut later = now;
        let mut put = |board: &mut Board, author: MemberId, id: u32, which: usize, len: usize| {
            later += Duration::from_secs(1);
            let mut bytes = vec![which as u8; len];
            bytes[0] = (which >> 8) as u8;
            let bytes: PictureBytes = Arc::from(bytes);
            let element = picture_element(id, picture_of(&bytes));
            let generation = board.generation_of(author);
            board.put_picture(author, generation, element, &bytes, later, None)
        };
        // Three each.
        for id in 0..3 {
            put(&mut board, BIA, id, id as usize, 10).unwrap();
        }
        assert_eq!(
            put(&mut board, BIA, 3, 3, 10),
            Err(Refusal::TooManyPictures)
        );
        // Replacing one of its own is no more.
        put(&mut board, BIA, 2, 4, 10).unwrap();
        // The board's bytes: 8 MB.
        let mb = MAX_PICTURE_BYTES;
        let mut which = 10;
        let mut taken = 0;
        'members: for m in 3..20u8 {
            for id in 0..3 {
                which += 1;
                match put(&mut board, MemberId([m; 32]), id, which, mb) {
                    Ok(_) => taken += 1,
                    Err(Refusal::TooManyPictures) => break 'members,
                    Err(e) => panic!("{e:?}"),
                }
            }
        }
        assert_eq!(
            taken,
            MAX_BOARD_PICTURE_BYTES / mb - 1,
            "30 bytes of Bia's too"
        );
        assert!(board.picture_bytes() <= MAX_BOARD_PICTURE_BYTES);
        // ...and its count: 16, small ones too.
        let mut board = Board::new(true);
        board.set_pictures(true);
        let mut taken = 0;
        'count: for m in 3..20u8 {
            for id in 0..3 {
                which += 1;
                match put(&mut board, MemberId([m; 32]), id, which, 10) {
                    Ok(_) => taken += 1,
                    Err(Refusal::TooManyPictures) => break 'count,
                    Err(e) => panic!("{e:?}"),
                }
            }
        }
        assert_eq!(taken, MAX_BOARD_PICTURES);
    }

    #[test]
    fn pictures_off_takes_them_off_the_board() {
        let now = Instant::now();
        let mut board = Board::new(true);
        assert_eq!(board.set_pictures(true), (true, Vec::new()));
        let bytes = test_picture(1);
        let generation = board.generation_of(BIA);
        board
            .put_picture(
                BIA,
                generation,
                picture_element(4, picture_of(&bytes)),
                &bytes,
                now,
                None,
            )
            .unwrap();
        board.go(BIA, &[BoardOp::Put(rect(1))], now).unwrap();
        let (changed, removed) = board.set_pictures(false);
        assert!(changed);
        assert_eq!(removed, [(BIA, vec![4])]);
        assert_eq!(board.len(), 1, "the rest stays");
        assert!(board.picture_ids().is_empty() && board.picture_bytes() == 0);
        assert_eq!(board.set_pictures(false), (false, Vec::new()));
    }

    // ---- The property test ----

    fn keys(snapshot: &[Authored]) -> BTreeSet<(MemberId, u32)> {
        snapshot.iter().map(|a| (a.author, a.element.id)).collect()
    }

    #[derive(Debug, Clone)]
    enum Step {
        /// A member's batch, maybe made before the last clear.
        Ops(u8, bool, Vec<BoardOp>),
        Wait(u16),
        Stop(u8, bool),
        Clear,
        Expire,
        /// The member is gone: the host finishes what it left open.
        Finish(u8),
        /// A member's picture (M27e): element id, which picture, maybe stale,
        /// maybe bytes that aren't what it names.
        Picture(u8, bool, u32, usize, bool),
        /// Pictures allowed or not.
        Pictures(bool),
    }

    /// The pictures the property test puts, small: the byte limits have
    /// tests of their own.
    fn test_picture(which: usize) -> PictureBytes {
        Arc::from(vec![which as u8; 10 + which * 7])
    }

    fn picture_of(bytes: &PictureBytes) -> PictureId {
        PictureId(Sha256::digest(bytes).into())
    }

    fn picture_element(id: u32, picture: PictureId) -> Element {
        Element {
            kind: ElementKind::Picture,
            points: vec![Point::new(id as u16, 10), Point::new(id as u16 + 500, 900)],
            picture: Some(picture),
            ..rect(id)
        }
    }

    fn element() -> impl Strategy<Value = Element> {
        (
            0u32..12,
            prop_oneof![
                Just(ElementKind::Pen),
                Just(ElementKind::Highlighter),
                Just(ElementKind::Arrow),
                Just(ElementKind::Rectangle),
                Just(ElementKind::Text),
                Just(ElementKind::Picture),
            ],
            any::<bool>(),
            any::<bool>(),
            1usize..6,
            prop_oneof![Just("Hi"), Just("Olá\nali"), Just("😀 x"), Just("")],
            0usize..4,
        )
            .prop_map(|(id, kind, fades, done, n, text, which)| Element {
                id,
                kind,
                colour: (id % 8) as u8,
                width: (id % 3) as u8,
                fades,
                stay: 4,
                done: done || !(kind.is_stroke() || kind == ElementKind::Text),
                points: (0..if kind.is_stroke() { n } else { 2 })
                    .map(|i| Point::new(i as u16 * 100, id as u16))
                    .collect(),
                // Now and then a box with no text, which is refused.
                text: if kind == ElementKind::Text {
                    text.into()
                } else {
                    String::new()
                },
                // A picture moved: taken only if it is already there.
                picture: (kind == ElementKind::Picture).then(|| picture_of(&test_picture(which))),
            })
            .prop_map(|e| match e.kind {
                ElementKind::Picture => Element {
                    points: vec![Point::new(0, 0), Point::new(e.id as u16 + 1, 7)],
                    ..e
                },
                _ => e,
            })
    }

    fn op() -> impl Strategy<Value = BoardOp> {
        prop_oneof![
            4 => element().prop_map(BoardOp::Put),
            3 => (0u32..12, 0usize..4, any::<bool>()).prop_map(|(id, n, done)| BoardOp::Append {
                id,
                points: vec![Point::new(3, 3); n],
                done,
            }),
            2 => prop::collection::vec(0u32..12, 1..4).prop_map(BoardOp::Remove),
            1 => Just(BoardOp::Ping(Point::new(5, 5))),
        ]
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            10 => (0u8..3, prop::bool::weighted(0.1), prop::collection::vec(op(), 1..5))
                .prop_map(|(m, stale, ops)| Step::Ops(m, stale, ops)),
            3 => (0u16..8000).prop_map(Step::Wait),
            1 => (0u8..3, any::<bool>()).prop_map(|(m, s)| Step::Stop(m, s)),
            1 => Just(Step::Clear),
            2 => Just(Step::Expire),
            1 => (0u8..3).prop_map(Step::Finish),
            4 => (0u8..3, prop::bool::weighted(0.1), 0u32..12, 0usize..4, prop::bool::weighted(0.1))
                .prop_map(|(m, stale, id, which, wrong)| Step::Picture(m, stale, id, which, wrong)),
            1 => any::<bool>().prop_map(Step::Pictures),
        ]
    }

    proptest! {
        /// Whatever members send: each changes only its own elements, a
        /// stopped member changes nothing, the limits and counts hold, a
        /// viewer applying every update sees the host's board, and so does a
        /// newcomer from a snapshot; finished fading elements leave in time.
        #[test]
        fn the_board_holds(steps in prop::collection::vec(step(), 1..80)) {
            let mut now = Instant::now();
            let mut board = Board::new(true);
            board.set_pictures(true);
            let mut viewer = Replica::default();
            for step in steps {
                match step {
                    Step::Ops(m, stale, ops) => {
                        let author = MemberId([m; 32]);
                        let others: Vec<Authored> = board
                            .snapshot()
                            .into_iter()
                            .filter(|a| a.author != author)
                            .collect();
                        let before = board.snapshot();
                        let stopped = board.is_stopped(author);
                        let generation =
                            board.generation_of(author).wrapping_sub(u32::from(stale));
                        match board.apply(author, generation, &ops, now, None) {
                            Ok(_) => {
                                prop_assert!(!stopped && !stale);
                                viewer.heard(author, &ops);
                            }
                            Err(_) => prop_assert_eq!(board.snapshot(), before),
                        }
                        let after: Vec<Authored> = board
                            .snapshot()
                            .into_iter()
                            .filter(|a| a.author != author)
                            .collect();
                        prop_assert_eq!(after, others);
                    }
                    Step::Wait(ms) => now += Duration::from_millis(u64::from(ms)),
                    Step::Stop(m, stopped) => {
                        let member = MemberId([m; 32]);
                        board.set_stopped(member, stopped);
                        if stopped {
                            viewer.cleared(board.generation_of(member), Some(member), None);
                        }
                    }
                    Step::Clear => {
                        board.clear();
                        viewer.cleared(board.generation(), None, None);
                    }
                    Step::Expire => {
                        board.expire(now);
                        // Windows fade them themselves; the host's drop
                        // only keeps newcomers from getting them.
                        let kept = keys(&board.snapshot());
                        viewer.retain(|a, e| kept.contains(&(a, e.id)) || !(e.done && e.fades));
                    }
                    Step::Finish(m) => {
                        let member = MemberId([m; 32]);
                        let ops = board.finish_open(member, now);
                        viewer.heard(member, &ops);
                        prop_assert!(board.snapshot().iter().all(|a| a.author != member || a.element.done));
                    }
                    Step::Picture(m, stale, id, which, wrong) => {
                        let author = MemberId([m; 32]);
                        let before = board.snapshot();
                        let generation =
                            board.generation_of(author).wrapping_sub(u32::from(stale));
                        let bytes = test_picture(which);
                        let named = if wrong { picture_of(&test_picture(which + 1)) } else { picture_of(&bytes) };
                        let element = picture_element(id, named);
                        match board.put_picture(author, generation, element.clone(), &bytes, now, None) {
                            Ok(_) => {
                                prop_assert!(!stale && !wrong && board.pictures_on());
                                prop_assert!(!board.is_stopped(author));
                                viewer.heard(author, &[BoardOp::Put(element)]);
                            }
                            Err(_) => prop_assert_eq!(board.snapshot(), before),
                        }
                    }
                    Step::Pictures(on) => {
                        let (_, removed) = board.set_pictures(on);
                        for (author, ids) in removed {
                            viewer.heard(author, &[BoardOp::Remove(ids)]);
                        }
                    }
                }
                // The counts are the board's.
                let snapshot = board.snapshot();
                prop_assert_eq!(snapshot.len(), board.len());
                prop_assert_eq!(
                    snapshot.iter().map(|a| a.element.load()).sum::<usize>(),
                    board.points()
                );
                prop_assert!(board.len() <= MAX_BOARD_ELEMENTS);
                prop_assert!(board.points() <= MAX_BOARD_POINTS);
                for (member, n) in board.counts() {
                    prop_assert!(n <= MAX_MEMBER_ELEMENTS);
                    prop_assert_eq!(n, snapshot.iter().filter(|a| a.author == member).count());
                }
                prop_assert!(snapshot.iter().all(|a| a.element.is_valid()));
                // Each picture is held exactly while an element shows it, and
                // the counts are the board's.
                let shown: BTreeSet<PictureId> =
                    snapshot.iter().filter_map(|a| a.element.picture).collect();
                prop_assert_eq!(&board.picture_ids().into_iter().collect::<BTreeSet<_>>(), &shown);
                prop_assert_eq!(
                    board.picture_bytes(),
                    shown.iter().map(|id| board.picture(id).unwrap().len()).sum::<usize>()
                );
                for (_, n) in board.pictures.values() {
                    prop_assert!(*n > 0);
                }
                prop_assert_eq!(
                    board.pictures_of.values().sum::<usize>(),
                    snapshot.iter().filter(|a| a.element.picture.is_some()).count()
                );
                for (member, n) in &board.pictures_of {
                    prop_assert!(*n <= MAX_MEMBER_PICTURES);
                    prop_assert_eq!(*n, snapshot.iter().filter(|a| a.author == *member && a.element.picture.is_some()).count());
                }
                if !board.pictures_on() {
                    prop_assert!(shown.is_empty());
                }
                // A viewer that heard every update, and a newcomer, agree,
                // in the drawing order too.
                let mut newcomer = Replica::default();
                for part in BoardSnapshot::parts(SessionId(1), board.generation(), snapshot.clone()) {
                    newcomer.snapshot_part(part.generation, part.first, &part.elements);
                }
                prop_assert_eq!(&newcomer.snapshot(), &snapshot);
                prop_assert_eq!(&viewer.snapshot(), &snapshot);
                prop_assert_eq!(newcomer.generation(), board.generation());
                // Nothing finished and fading outstays its time.
                if let Some(next) = board.next_expiry() {
                    prop_assert!(next <= now + FADE_DROP);
                }
            }
        }
    }
}
