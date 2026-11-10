//! The drawing cases (ADR 0013's "shared cases"): what each tool and key
//! does to our own elements and what goes to the host. M27b's board overlay
//! checks the same cases in its own editor.

use couch_proto::board::valid_batch;

use super::*;

const ME: MemberId = MemberId([1; 32]);
const BIA: MemberId = MemberId([2; 32]);

fn p(x: u16, y: u16) -> Point {
    Point::new(x, y)
}

struct Desk {
    editor: Editor,
    board: BoardView,
    now: Instant,
    sent: Vec<BoardOp>,
}

impl Desk {
    fn new() -> Self {
        let mut editor = Editor::new(ME);
        editor.set_aspect(1920, 1080);
        Self {
            editor,
            board: BoardView::default(),
            now: Instant::now(),
            sent: Vec::new(),
        }
    }

    fn tool(&mut self, tool: DrawTool) {
        self.editor.set_tool(tool, &mut self.board, self.now);
    }

    fn later(&mut self, ms: u64) {
        self.now += Duration::from_millis(ms);
    }

    /// A press, drags through `through`, a release at the last.
    fn stroke(&mut self, points: &[Point], shift: bool) {
        let (first, rest) = points.split_first().unwrap();
        self.editor.press(*first, shift, &mut self.board, self.now);
        for at in rest {
            self.later(10);
            self.editor.drag(*at, shift, &mut self.board, self.now);
        }
        self.editor
            .release(rest.last().copied(), shift, &mut self.board, self.now);
    }

    fn key(&mut self, action: DrawAction) -> Keyed {
        self.editor.key(action, false, &mut self.board, self.now)
    }

    /// Everything queued goes out, batch by batch, as the window sends it.
    fn send(&mut self) {
        loop {
            self.later(40);
            match self.editor.take_batch(self.now) {
                Some((_, batch)) => {
                    assert!(valid_batch(&batch), "{batch:?}");
                    self.sent.extend(batch);
                }
                None if !self.editor.pending() => break,
                None => {}
            }
        }
    }

    fn mine(&self) -> Vec<Element> {
        self.board.of(ME).map(|s| s.element.clone()).collect()
    }

    /// What the host would hold of ours, from what we sent.
    fn hosts_copy(&self) -> Vec<Element> {
        let mut replica = couch_room_like::Replica::default();
        replica.heard(&self.sent);
        replica.elements
    }
}

/// The host's copy, as `couch_room::board::Replica` keeps it (the viewer
/// doesn't link couch-room): checks that what we show is what we sent.
mod couch_room_like {
    use super::*;

    #[derive(Default)]
    pub struct Replica {
        pub elements: Vec<Element>,
    }

    impl Replica {
        pub fn heard(&mut self, ops: &[BoardOp]) {
            for op in ops {
                match op {
                    BoardOp::Put(e) => match self.elements.iter_mut().find(|x| x.id == e.id) {
                        Some(x) => *x = e.clone(),
                        None => self.elements.push(e.clone()),
                    },
                    BoardOp::Append { id, points, done } => {
                        let e = self.elements.iter_mut().find(|x| x.id == *id).unwrap();
                        assert!(!e.done, "points for a finished stroke");
                        e.points.extend_from_slice(points);
                        e.done = *done;
                    }
                    BoardOp::Remove(ids) => self.elements.retain(|x| !ids.contains(&x.id)),
                    _ => {}
                }
            }
        }
    }
}

#[test]
fn a_pen_stroke_goes_as_it_is_drawn_and_undoes_whole() {
    let mut d = Desk::new();
    let points: Vec<Point> = (0..20).map(|i| p(1000 + i * 400, 2000)).collect();
    d.editor.press(points[0], false, &mut d.board, d.now);
    d.later(40);
    // The start goes out at once, while drawing.
    let (_, first) = d.editor.take_batch(d.now).unwrap();
    assert!(matches!(&first[..], [BoardOp::Put(e)] if !e.done));
    d.sent.extend(first);
    for at in &points[1..] {
        d.later(10);
        d.editor.drag(*at, false, &mut d.board, d.now);
    }
    d.editor
        .release(Some(points[19]), false, &mut d.board, d.now);
    d.send();
    let mine = d.mine();
    assert_eq!(mine.len(), 1);
    assert!(mine[0].done && mine[0].points.len() == 20);
    assert_eq!(d.hosts_copy(), mine, "what we show is what we sent");
    // Batches never come closer than the host takes them.
    d.key(DrawAction::Undo);
    d.send();
    assert!(d.mine().is_empty());
    assert_eq!(d.hosts_copy(), d.mine());
    d.key(DrawAction::Redo);
    d.send();
    assert_eq!(d.mine(), mine);
    assert_eq!(d.hosts_copy(), mine);
}

#[test]
fn shift_draws_squares_and_lines_at_fifteen_degrees() {
    let mut d = Desk::new();
    d.tool(DrawTool::Rectangle);
    d.stroke(&[p(10_000, 10_000), p(20_000, 14_000)], true);
    let rect = &d.mine()[0];
    let (a, b) = (rect.points[0], rect.points[1]);
    // Square on screen: as wide as tall in picture heights (16:9).
    let w = f32::from(b.x - a.x) / 65_535.0 * 16.0 / 9.0;
    let h = f32::from(b.y - a.y) / 65_535.0;
    assert!((w - h).abs() < 0.001, "{w} {h}");
    d.tool(DrawTool::Line);
    d.stroke(&[p(10_000, 30_000), p(30_000, 30_900)], true);
    let line = &d.mine()[1];
    assert_eq!(line.points[0].y, line.points[1].y, "flat");
    // Without Shift, as drawn.
    d.tool(DrawTool::Arrow);
    d.stroke(&[p(1, 2), p(3_000, 4_000)], false);
    assert_eq!(d.mine()[2].points, [p(1, 2), p(3_000, 4_000)]);
    assert_eq!(d.mine()[2].kind, ElementKind::Arrow);
}

#[test]
fn the_eraser_takes_only_ours_and_undoes() {
    let mut d = Desk::new();
    // Bia's box, under ours.
    let theirs = Element {
        id: 1,
        kind: ElementKind::Rectangle,
        colour: 0,
        width: 0,
        fades: false,
        done: true,
        points: vec![p(10_000, 10_000), p(20_000, 20_000)],
    };
    d.board.apply(BIA, &[BoardOp::Put(theirs)], d.now);
    d.tool(DrawTool::Rectangle);
    d.stroke(&[p(10_000, 10_000), p(20_000, 20_000)], false);
    d.tool(DrawTool::Eraser);
    d.stroke(&[p(10_000, 15_000), p(10_050, 15_000)], false);
    assert!(d.mine().is_empty());
    assert!(d.board.get(BIA, 1).is_some(), "theirs stays");
    d.send();
    assert!(d.hosts_copy().is_empty(), "the host hears it erased");
    d.key(DrawAction::Undo);
    assert_eq!(d.mine().len(), 1, "erasing undone");
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
}

#[test]
fn selecting_moving_and_nudging_keep_inside_the_picture() {
    let mut d = Desk::new();
    d.tool(DrawTool::Rectangle);
    d.stroke(&[p(1_000, 1_000), p(5_000, 5_000)], false);
    d.tool(DrawTool::Select);
    // Grab it inside, move it.
    d.stroke(&[p(3_000, 3_000), p(13_000, 3_000)], false);
    assert_eq!(d.mine()[0].points[0], p(11_000, 1_000));
    assert_eq!(d.editor.selection().len(), 1);
    // Nudged up, past the edge: stops at it.
    d.editor
        .key(DrawAction::Nudge(0, -1), true, &mut d.board, d.now);
    assert_eq!(d.mine()[0].points[0].y, 0);
    d.key(DrawAction::Undo);
    assert_eq!(d.mine()[0].points[0], p(11_000, 1_000));
    d.key(DrawAction::Undo);
    assert_eq!(d.mine()[0].points[0], p(1_000, 1_000), "the move undone");
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
    // A marquee around it selects it; one around nothing, nothing.
    d.stroke(&[p(60_000, 60_000), p(61_000, 61_000)], false);
    assert!(d.editor.selection().is_empty());
    d.stroke(&[p(0, 0), p(6_000, 6_000)], false);
    assert_eq!(d.editor.selection().len(), 1);
    // Esc: the selection, then the tool, then drawing.
    assert_eq!(d.key(DrawAction::Leave), Keyed::Done);
    assert!(d.editor.selection().is_empty());
    assert_eq!(d.key(DrawAction::Leave), Keyed::LeaveDrawing);
}

#[test]
fn copy_paste_and_duplicate_make_new_ones_of_ours() {
    let mut d = Desk::new();
    d.tool(DrawTool::Ellipse);
    d.stroke(&[p(1_000, 1_000), p(4_000, 3_000)], false);
    d.key(DrawAction::SelectAll);
    d.key(DrawAction::Copy);
    d.key(DrawAction::Paste);
    d.key(DrawAction::Paste);
    let mine = d.mine();
    assert_eq!(mine.len(), 3);
    let ids: BTreeSet<u32> = mine.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), 3, "each its own number");
    assert_ne!(mine[1].points, mine[2].points, "each paste lands further");
    assert_eq!(d.editor.selection().len(), 1, "the paste is selected");
    d.key(DrawAction::Duplicate);
    assert_eq!(d.mine().len(), 4);
    d.key(DrawAction::SelectAll);
    d.key(DrawAction::Delete);
    assert!(d.mine().is_empty());
    d.key(DrawAction::Undo);
    assert_eq!(d.mine().len(), 4);
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
}

#[test]
fn a_colour_picked_recolours_the_selection() {
    let mut d = Desk::new();
    d.tool(DrawTool::Line);
    d.stroke(&[p(1_000, 1_000), p(4_000, 3_000)], false);
    d.key(DrawAction::SelectAll);
    d.editor
        .set_style(Some(5), Some(2), Some(true), &mut d.board, d.now);
    let e = &d.mine()[0];
    assert_eq!((e.colour, e.width, e.fades), (5, 2, true));
    d.key(DrawAction::Undo);
    assert_eq!(d.mine()[0].colour, 0);
    // And what's drawn next takes it.
    d.tool(DrawTool::Line);
    d.stroke(&[p(1_000, 1_000), p(4_000, 3_000)], false);
    assert_eq!(d.mine()[1].colour, 5);
}

#[test]
fn the_laser_and_pings_keep_to_the_hosts_rates() {
    let mut d = Desk::new();
    d.tool(DrawTool::Laser);
    d.editor.press(p(0, 0), false, &mut d.board, d.now);
    // A drag every 5 ms for a second: at most 50 points.
    for i in 0..200u16 {
        d.now += Duration::from_millis(5);
        d.editor.drag(p(i * 100, 0), false, &mut d.board, d.now);
    }
    d.editor.release(Some(p(0, 0)), false, &mut d.board, d.now);
    d.send();
    let marks: usize = d.sent.iter().map(BoardOp::marks).sum();
    assert!(marks <= 51, "{marks}");
    assert!(matches!(
        d.sent.first(),
        Some(BoardOp::Laser { start: true, .. })
    ));
    assert!(d.mine().is_empty(), "nothing stays");
    // In the colour picked: red until another is.
    assert!(
        d.sent
            .iter()
            .all(|op| matches!(op, BoardOp::Laser { colour: 0, .. }))
    );
    d.sent.clear();
    d.editor.set_style(Some(4), None, None, &mut d.board, d.now);
    d.editor.press(p(0, 0), false, &mut d.board, d.now);
    d.editor.release(Some(p(0, 0)), false, &mut d.board, d.now);
    d.send();
    assert!(matches!(d.sent[..], [BoardOp::Laser { colour: 4, .. }]));
    d.sent.clear();
    for _ in 0..10 {
        d.later(100);
        d.editor.ping(p(5, 5), &mut d.board, d.now);
    }
    d.send();
    assert_eq!(
        d.sent
            .iter()
            .filter(|op| matches!(op, BoardOp::Ping(_)))
            .count(),
        2
    );
}

#[test]
fn a_long_stroke_goes_on_in_a_new_element() {
    let mut d = Desk::new();
    let points: Vec<Point> = (0..(MAX_STROKE_POINTS as u32 + 100))
        .map(|i| p((i % 600) as u16 * 100, (i / 600) as u16 * 3_000))
        .collect();
    d.editor.press(points[0], false, &mut d.board, d.now);
    for at in &points[1..] {
        d.editor.drag(*at, false, &mut d.board, d.now);
    }
    d.editor.release(None, false, &mut d.board, d.now);
    d.send();
    let mine = d.mine();
    assert_eq!(mine.len(), 2);
    assert!(
        mine.iter()
            .all(|e| e.done && e.points.len() <= MAX_STROKE_POINTS)
    );
    assert_eq!(d.hosts_copy(), mine);
    d.key(DrawAction::Undo);
    assert!(d.mine().is_empty(), "one step for the whole stroke");
}

#[test]
fn we_stop_at_our_limit() {
    let mut d = Desk::new();
    d.tool(DrawTool::Line);
    for i in 0..MAX_MEMBER_ELEMENTS as u16 {
        d.stroke(&[p(i, 0), p(i, 100)], false);
    }
    assert_eq!(d.editor.take_note(), None);
    d.stroke(&[p(0, 0), p(0, 100)], false);
    assert_eq!(d.mine().len(), MAX_MEMBER_ELEMENTS);
    assert!(d.editor.take_note().is_some());
    d.send();
    assert_eq!(d.hosts_copy().len(), MAX_MEMBER_ELEMENTS);
}

#[test]
fn what_was_made_before_a_clear_goes_with_the_generation_it_was_made_in() {
    let mut d = Desk::new();
    let cleared = |author, generation| {
        couch_proto::ipc::BoardNews::Cleared(couch_proto::board::BoardCleared {
            session: couch_proto::ids::SessionId(1),
            generation,
            author,
        })
    };
    d.tool(DrawTool::Line);
    d.stroke(&[p(0, 0), p(9, 9)], false);
    // A clear of someone else's: ours goes on in our generation.
    d.board.news(cleared(Some(BIA), 7), Some(ME), d.now);
    d.editor.sync(&d.board);
    d.later(40);
    assert_eq!(d.editor.take_batch(d.now).unwrap().0, 0);
    // A clear of ours arrives while a line waits to go: the line goes with
    // it (the host would drop it anyway), and what we make next is of the
    // new generation.
    d.stroke(&[p(0, 0), p(19, 19)], false);
    d.board.news(cleared(Some(ME), 8), Some(ME), d.now);
    d.editor.cleared(&d.board);
    d.later(40);
    assert_eq!(d.editor.take_batch(d.now), None);
    assert!(d.mine().is_empty());
    d.stroke(&[p(0, 0), p(29, 29)], false);
    d.later(40);
    assert_eq!(d.editor.take_batch(d.now).unwrap().0, 8);
}

/// The second review's finding: a window started again after a crash
/// mid-stroke gets the stroke back open; it finishes it, or nobody would.
#[test]
fn a_stroke_left_open_by_a_crash_is_finished() {
    let mut d = Desk::new();
    let open = Element {
        id: 5,
        kind: ElementKind::Pen,
        colour: 0,
        width: 0,
        fades: true,
        done: false,
        points: vec![p(1, 1)],
    };
    d.board.news(
        couch_proto::ipc::BoardNews::Snapshot(couch_proto::board::BoardSnapshot {
            session: couch_proto::ids::SessionId(1),
            generation: 3,
            first: true,
            elements: vec![couch_proto::board::Authored {
                author: ME,
                element: open,
            }],
        }),
        Some(ME),
        d.now,
    );
    d.editor.cleared(&d.board);
    d.editor.finish_strays(&mut d.board, d.now);
    assert!(d.mine()[0].done);
    d.later(40);
    let (generation, ops) = d.editor.take_batch(d.now).unwrap();
    assert_eq!(generation, 3);
    assert_eq!(
        ops,
        [BoardOp::Append {
            id: 5,
            points: vec![],
            done: true
        }]
    );
    // A stroke being drawn now is left alone.
    d.tool(DrawTool::Pen);
    d.editor.press(p(0, 0), false, &mut d.board, d.now);
    d.editor.finish_strays(&mut d.board, d.now);
    assert!(d.mine().iter().any(|e| !e.done));
}

#[test]
fn what_the_host_cleared_leaves_the_history() {
    let mut d = Desk::new();
    d.tool(DrawTool::Pen);
    d.editor.press(p(0, 0), false, &mut d.board, d.now);
    d.editor.drag(p(500, 500), false, &mut d.board, d.now);
    // Cleared mid-stroke: the stroke stops, nothing more of it goes.
    d.board.news(
        couch_proto::ipc::BoardNews::Cleared(couch_proto::board::BoardCleared {
            session: couch_proto::ids::SessionId(1),
            generation: 1,
            author: None,
        }),
        Some(ME),
        d.now,
    );
    d.editor.cleared(&d.board);
    d.editor.drag(p(900, 900), false, &mut d.board, d.now);
    d.editor
        .release(Some(p(900, 900)), false, &mut d.board, d.now);
    assert!(d.mine().is_empty());
    d.key(DrawAction::Undo);
    assert!(d.mine().is_empty(), "nothing to undo");
    // Our numbers stay new after a snapshot holding ours.
    d.board.apply(
        ME,
        &[BoardOp::Put(Element {
            id: 41,
            kind: ElementKind::Line,
            colour: 0,
            width: 0,
            fades: false,
            done: true,
            points: vec![p(0, 0), p(1, 1)],
        })],
        d.now,
    );
    d.editor.sync(&d.board);
    d.tool(DrawTool::Line);
    d.stroke(&[p(0, 0), p(9, 9)], false);
    assert!(d.board.get(ME, 42).is_some());
}
