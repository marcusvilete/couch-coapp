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
        stay: 4,
        done: true,
        points: vec![p(10_000, 10_000), p(20_000, 20_000)],
        text: String::new(),
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
        stay: 4,
        done: false,
        points: vec![p(1, 1)],
        text: String::new(),
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
            stay: 4,
            done: true,
            points: vec![p(0, 0), p(1, 1)],
            text: String::new(),
        })],
        d.now,
    );
    d.editor.sync(&d.board);
    d.tool(DrawTool::Line);
    d.stroke(&[p(0, 0), p(9, 9)], false);
    assert!(d.board.get(ME, 42).is_some());
}

/// Wire revision 16 (the author's request in M27b's run): how long a fading
/// stroke stays is its author's pick.
#[test]
fn picking_how_long_turns_fading_on_and_restyles_the_selection() {
    let mut d = Desk::new();
    assert_eq!(d.editor.stay, couch_proto::board::DEFAULT_STAY);
    d.tool(DrawTool::Line);
    d.stroke(&[p(1_000, 1_000), p(4_000, 3_000)], false);
    d.key(DrawAction::SelectAll);
    d.editor.set_stay(15, &mut d.board, d.now);
    let e = &d.mine()[0];
    assert!(e.fades && e.stay == 15, "{e:?}");
    d.editor.set_stay(5, &mut d.board, d.now);
    assert_eq!(d.editor.stay, 15, "only the stays offered");
    d.key(DrawAction::Undo);
    assert!(!d.mine()[0].fades && d.mine()[0].stay == 4);
    // What's drawn next fades after the stay picked.
    d.tool(DrawTool::Line);
    d.stroke(&[p(1_000, 1_000), p(4_000, 3_000)], false);
    assert!(d.mine()[1].fades && d.mine()[1].stay == 15);
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
}

/// The toolbar's one fade button (the author's choice in M27b's run): a
/// click goes round "stays", 2, 4, 8, 15 s; the wheel steps up or down and
/// stops at either end; the selection takes each step.
#[test]
fn the_fade_button_goes_round_and_the_wheel_steps() {
    let mut d = Desk::new();
    let state = |d: &Desk| (d.editor.fades, d.editor.stay);
    assert_eq!(state(&d), (false, 4), "stays, by default");
    for want in [(true, 2), (true, 4), (true, 8), (true, 15)] {
        d.editor.step_fade(FadeStep::Next, &mut d.board, d.now);
        assert_eq!(state(&d), want);
    }
    d.editor.step_fade(FadeStep::Next, &mut d.board, d.now);
    assert!(!d.editor.fades, "round to stays");
    d.editor.step_fade(FadeStep::Shorter, &mut d.board, d.now);
    assert!(!d.editor.fades, "the wheel stops at stays");
    d.editor.step_fade(FadeStep::Longer, &mut d.board, d.now);
    assert_eq!(state(&d), (true, 2));
    for _ in 0..6 {
        d.editor.step_fade(FadeStep::Longer, &mut d.board, d.now);
    }
    assert_eq!(state(&d), (true, 15), "and at the longest");
    d.editor.step_fade(FadeStep::Shorter, &mut d.board, d.now);
    assert_eq!(state(&d), (true, 8));
    // The selection follows.
    d.tool(DrawTool::Line);
    d.stroke(&[p(1_000, 1_000), p(4_000, 3_000)], false);
    d.key(DrawAction::SelectAll);
    d.editor.step_fade(FadeStep::Shorter, &mut d.board, d.now);
    assert!(d.mine()[0].fades && d.mine()[0].stay == 4);
    d.editor.step_fade(FadeStep::Shorter, &mut d.board, d.now);
    d.editor.step_fade(FadeStep::Shorter, &mut d.board, d.now);
    assert!(!d.mine()[0].fades, "stays now");
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
}

// ---- Text boxes (M27c) ----

impl Desk {
    /// A click with the Text tool.
    fn click_text(&mut self, at: Point) {
        self.tool(DrawTool::Text);
        self.editor.press(at, false, &mut self.board, self.now);
        self.editor
            .release(Some(at), false, &mut self.board, self.now);
    }

    fn type_text(&mut self, text: &str) {
        self.editor.type_text(text, &mut self.board, self.now);
    }

    fn text_key(&mut self, key: TextKey) {
        self.editor
            .text_key(key, false, false, &mut self.board, self.now);
    }

    /// A key with Shift and Ctrl as given.
    fn text_key_with(&mut self, key: TextKey, shift: bool, ctrl: bool) {
        self.editor
            .text_key(key, shift, ctrl, &mut self.board, self.now);
    }

    /// The point over character `col` of `line` in the box at `anchor`
    /// (medium text, the estimate's half an em a character).
    fn over_char(&self, anchor: Point, line: usize, col: usize) -> Point {
        let size = 0.04_f32;
        let x = col as f32 * 0.5 * size / (16.0 / 9.0) * 65_535.0;
        let y = (line as f32 + 0.5) * 1.25 * size * 65_535.0;
        p(anchor.x + x.round() as u16, anchor.y + y.round() as u16)
    }

    fn text_now(&self) -> String {
        self.board
            .of(ME)
            .last()
            .map_or(String::new(), |s| s.element.text.clone())
    }
}

/// Within one of `want`: the two editors round alike but for halves.
fn near(got: Point, want: Point) -> bool {
    got.x.abs_diff(want.x) <= 1 && got.y.abs_diff(want.y) <= 1
}

#[test]
fn a_text_box_goes_as_it_is_typed_and_finishes_as_one_step() {
    let mut d = Desk::new();
    d.click_text(p(10_000, 20_000));
    assert!(d.editor.typing());
    d.send();
    assert!(d.sent.is_empty(), "nothing goes before it has text");
    d.type_text("Hi");
    d.send();
    assert!(
        matches!(&d.sent[..], [BoardOp::Put(e)] if e.text == "Hi" && !e.done),
        "{:?}",
        d.sent
    );
    d.text_key(TextKey::Enter);
    d.type_text("there");
    d.text_key(TextKey::Finish);
    assert!(!d.editor.typing());
    d.send();
    let mine = d.mine();
    assert_eq!(mine.len(), 1);
    let e = &mine[0];
    assert!(e.done && e.kind == ElementKind::Text && e.text == "Hi\nthere");
    // Its box: as wide as "there" (half an em a character, medium: 0.04 of
    // the height) and two lines tall, from where it was clicked.
    assert_eq!(e.points[0], p(10_000, 20_000));
    let wide = 5.0 * 0.5 * 0.04 / (16.0 / 9.0) * 65_535.0;
    let tall = 2.0 * 1.25 * 0.04 * 65_535.0;
    let end = p(
        10_000 + (wide as f32).round() as u16,
        20_000 + (tall as f32).round() as u16,
    );
    assert!(near(e.points[1], end), "{:?}", e.points);
    assert_eq!(d.hosts_copy(), mine, "what we show is what we sent");
    // Finished with Esc: selected, to move at once.
    assert_eq!(d.editor.tool, DrawTool::Select);
    assert_eq!(d.editor.selection().len(), 1);
    // One step: undone whole, and back.
    d.key(DrawAction::Undo);
    assert!(d.mine().is_empty());
    d.key(DrawAction::Redo);
    assert_eq!(d.mine(), mine);
    d.send();
    assert_eq!(d.hosts_copy(), mine);
}

#[test]
fn the_caret_moves_and_keys_edit_where_it_is() {
    let mut d = Desk::new();
    d.click_text(p(1_000, 1_000));
    d.type_text("abc\nde");
    let text = |d: &Desk| d.board.of(ME).next().unwrap().element.text.clone();
    d.text_key(TextKey::Up);
    d.type_text("X");
    assert_eq!(text(&d), "abXc\nde", "up, at the same place");
    d.text_key(TextKey::End);
    d.text_key(TextKey::Down);
    d.type_text("Y");
    assert_eq!(text(&d), "abXc\ndeY", "down, at most the line's end");
    d.text_key(TextKey::Home);
    d.text_key(TextKey::Backspace);
    assert_eq!(text(&d), "abXcdeY", "the line break went");
    d.text_key(TextKey::Delete);
    d.text_key(TextKey::Left);
    d.text_key(TextKey::Left);
    d.text_key(TextKey::Delete);
    assert_eq!(text(&d), "abceY");
    // Tabs as spaces, control characters never.
    d.text_key(TextKey::End);
    d.type_text("\t!\u{7}");
    assert_eq!(text(&d), "abceY    !");
    // Accents and wide characters are one character each.
    d.text_key(TextKey::Home);
    d.type_text("é😀");
    d.text_key(TextKey::Left);
    d.text_key(TextKey::Backspace);
    assert_eq!(text(&d), "😀abceY    !");
    d.text_key(TextKey::Finish);
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
}

#[test]
fn a_box_emptied_goes_and_spaces_alone_never_go() {
    let mut d = Desk::new();
    d.click_text(p(1_000, 1_000));
    d.type_text("  ");
    d.send();
    assert!(d.sent.is_empty(), "spaces alone are nothing");
    d.type_text("x");
    d.send();
    assert_eq!(d.hosts_copy().len(), 1);
    for _ in 0..3 {
        d.text_key(TextKey::Backspace);
    }
    d.send();
    assert!(d.hosts_copy().is_empty(), "emptied, it goes");
    assert!(d.mine().is_empty());
    d.text_key(TextKey::Finish);
    // A new box that came to nothing leaves no step to undo.
    d.tool(DrawTool::Rectangle);
    d.stroke(&[p(1_000, 1_000), p(2_000, 2_000)], false);
    d.key(DrawAction::Undo);
    d.key(DrawAction::Undo);
    assert!(d.mine().is_empty());
    d.key(DrawAction::Redo);
    assert_eq!(d.mine().len(), 1, "the box's step was the rectangle's");
}

#[test]
fn editing_a_box_changes_it_as_one_step() {
    let mut d = Desk::new();
    d.click_text(p(10_000, 10_000));
    d.type_text("Hello");
    d.text_key(TextKey::Finish);
    let before = d.mine();
    // Double-clicked with Select, at the end of its line: typing in it,
    // the caret there, unfinished meanwhile.
    d.tool(DrawTool::Select);
    let at = p(before[0].points[1].x, 10_100);
    d.stroke(&[at], false);
    d.stroke(&[at], false);
    d.editor.double_click(at, &mut d.board, d.now);
    assert!(d.editor.typing());
    assert!(!d.mine()[0].done, "being typed, it doesn't fade");
    d.type_text("!");
    assert_eq!(d.mine()[0].text, "Hello!");
    // A click inside it moves the caret: to the start here.
    d.editor
        .press(p(10_000, 10_100), false, &mut d.board, d.now);
    d.type_text(">");
    assert_eq!(d.mine()[0].text, ">Hello!");
    // A click away finishes it, the tool staying.
    d.editor
        .press(p(60_000, 60_000), false, &mut d.board, d.now);
    assert!(!d.editor.typing());
    assert_eq!(d.editor.tool, DrawTool::Text);
    assert!(d.mine()[0].done);
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
    d.key(DrawAction::Undo);
    assert_eq!(d.mine(), before, "the edit undone whole");
    d.key(DrawAction::Redo);
    assert_eq!(d.mine()[0].text, ">Hello!");
    // The Text tool's click on our box edits it too; on theirs, a new one.
    let theirs = Element {
        id: 9,
        kind: ElementKind::Text,
        colour: 0,
        width: 1,
        fades: false,
        stay: 4,
        done: true,
        points: vec![p(40_000, 40_000), p(50_000, 45_000)],
        text: "Bia".into(),
    };
    d.board.apply(BIA, &[BoardOp::Put(theirs)], d.now);
    d.click_text(p(45_000, 42_000));
    d.type_text("mine");
    d.text_key(TextKey::Finish);
    assert_eq!(d.board.get(BIA, 9).unwrap().element.text, "Bia");
    assert_eq!(d.mine().len(), 2);
}

#[test]
fn text_stops_at_its_limit() {
    let mut d = Desk::new();
    d.click_text(p(1_000, 1_000));
    d.type_text(&"a".repeat(MAX_TEXT_CHARS - 1));
    d.type_text("bcd");
    let e = &d.mine()[0];
    assert_eq!(e.text.chars().count(), MAX_TEXT_CHARS);
    assert!(e.text.ends_with('b'));
    assert_eq!(d.editor.take_note(), Some(TOO_LONG));
    assert!(TOO_LONG.contains(&MAX_TEXT_CHARS.to_string()));
    d.text_key(TextKey::Enter);
    assert_eq!(d.mine()[0].text.chars().count(), MAX_TEXT_CHARS);
    d.text_key(TextKey::Finish);
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
}

#[test]
fn pasted_text_becomes_a_box_and_our_own_copy_stays_ours() {
    let mut d = Desk::new();
    d.tool(DrawTool::Rectangle);
    d.stroke(&[p(1_000, 1_000), p(4_000, 3_000)], false);
    d.key(DrawAction::SelectAll);
    // No text boxes in it: the clipboard gets nothing, and a paste of
    // nothing is our copy.
    assert_eq!(d.editor.copy(&d.board).as_deref(), Some(""));
    d.editor.paste(Some(""), None, &mut d.board, d.now);
    assert_eq!(d.mine().len(), 2);
    assert_eq!(d.mine()[1].kind, ElementKind::Rectangle);
    // Text copied elsewhere since: a box at the pointer, cleaned, finished
    // and selected, one step.
    d.editor.paste(
        Some("hello\r\nworld\t!\n\n"),
        Some(p(20_000, 20_000)),
        &mut d.board,
        d.now,
    );
    let e = d.mine()[2].clone();
    assert_eq!(e.text, "hello\nworld    !");
    assert!(e.done && e.points[0] == p(20_000, 20_000));
    assert_eq!(d.editor.selection(), &BTreeSet::from([e.id]));
    assert_eq!(d.editor.tool, DrawTool::Select);
    d.key(DrawAction::Undo);
    assert_eq!(d.mine().len(), 2);
    d.key(DrawAction::Redo);
    // Copied, its text goes on the clipboard; that coming back (the
    // clipboard's line breaks aside) pastes our copy, not a new box.
    d.key(DrawAction::SelectAll);
    assert_eq!(
        d.editor.copy(&d.board).as_deref(),
        Some("hello\nworld    !")
    );
    d.editor
        .paste(Some("hello\r\nworld    !"), None, &mut d.board, d.now);
    assert_eq!(d.mine().len(), 6, "all three again");
    // Near the edge it moves in, whole.
    d.editor.paste(
        Some("far right"),
        Some(p(65_000, 65_000)),
        &mut d.board,
        d.now,
    );
    let e = d.mine().last().unwrap().clone();
    assert_eq!(e.text, "far right");
    // Its whole size (nine characters, one line), ending at the edges.
    let wide = (9.0 * 0.5 * 0.04 / (16.0 / 9.0) * 65_535.0_f32).round() as u16;
    let tall = (1.25 * 0.04 * 65_535.0_f32).round() as u16;
    assert!(
        near(e.points[0], p(65_535 - wide, 65_535 - tall)),
        "{:?}",
        e.points
    );
    // Too long: cut, and said.
    d.editor
        .paste(Some(&"z".repeat(600)), None, &mut d.board, d.now);
    assert_eq!(d.mine().last().unwrap().text.len(), MAX_TEXT_CHARS);
    assert_eq!(d.editor.take_note(), Some(TOO_LONG));
    // While typing, it goes into the box, at the caret.
    d.click_text(p(1_000, 30_000));
    d.type_text("ab");
    d.text_key(TextKey::Left);
    d.editor.paste(Some("X\r\nY"), None, &mut d.board, d.now);
    assert_eq!(d.mine().last().unwrap().text, "aX\nYb");
    d.text_key(TextKey::Finish);
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
}

#[test]
fn a_text_box_left_open_is_finished_whole_and_a_clear_ends_typing() {
    let mut d = Desk::new();
    // As a window started again finds it: being typed.
    let open = Element {
        id: 3,
        kind: ElementKind::Text,
        colour: 0,
        width: 1,
        fades: true,
        stay: 4,
        done: false,
        points: vec![p(1, 1), p(5_000, 5_000)],
        text: "half".into(),
    };
    d.board.apply(ME, &[BoardOp::Put(open.clone())], d.now);
    d.sent.push(BoardOp::Put(open));
    d.editor.cleared(&d.board);
    d.editor.finish_strays(&mut d.board, d.now);
    d.send();
    assert!(
        matches!(d.sent.last(), Some(BoardOp::Put(e)) if e.done && e.text == "half"),
        "the box again, finished (an append is a stroke's alone): {:?}",
        d.sent
    );
    assert!(d.mine()[0].done);
    // The host clears ours while we type (its resync after a refused
    // batch, say): typing goes on, and the box comes back whole at the
    // next key, its old self gone from the history (the review's finding).
    d.click_text(p(30_000, 30_000));
    d.type_text("not gone");
    d.board.news(
        couch_proto::ipc::BoardNews::Cleared(couch_proto::board::BoardCleared {
            session: couch_proto::ids::SessionId(1),
            generation: 9,
            author: Some(ME),
        }),
        Some(ME),
        d.now,
    );
    d.editor.cleared(&d.board);
    assert!(d.editor.typing());
    assert!(d.mine().is_empty());
    d.type_text("!");
    assert_eq!(d.mine()[0].text, "not gone!");
    // Cleared again, then finished at once: it goes out with all its text.
    d.board.news(
        couch_proto::ipc::BoardNews::Cleared(couch_proto::board::BoardCleared {
            session: couch_proto::ids::SessionId(1),
            generation: 10,
            author: Some(ME),
        }),
        Some(ME),
        d.now,
    );
    d.editor.cleared(&d.board);
    d.text_key(TextKey::Finish);
    assert!(d.mine()[0].done && d.mine()[0].text == "not gone!");
    d.send();
    assert_eq!(d.hosts_copy().last().unwrap().text, "not gone!");
    // One step, new: undone, it goes; nothing older comes back.
    d.key(DrawAction::Undo);
    assert!(d.mine().is_empty());
}

#[test]
fn a_size_picked_for_a_selected_box_measures_it_again() {
    let mut d = Desk::new();
    d.click_text(p(1_000, 1_000));
    d.type_text("Hello");
    d.text_key(TextKey::Finish);
    let small = d.mine()[0].points[1];
    d.editor.set_style(None, Some(2), None, &mut d.board, d.now);
    let e = d.mine()[0].clone();
    assert_eq!(e.points[0], p(1_000, 1_000), "its corner stays");
    let wide = (5.0 * 0.5 * 0.065 / (16.0 / 9.0) * 65_535.0_f32).round() as u16;
    let tall = (1.25 * 0.065 * 65_535.0_f32).round() as u16;
    assert!(e.points[1].x > small.x);
    assert!(
        near(e.points[1], p(1_000 + wide, 1_000 + tall)),
        "{:?}",
        e.points
    );
    d.key(DrawAction::Undo);
    assert_eq!(d.mine()[0].points[1], small);
}

#[test]
fn lines_stop_at_their_limit_and_only_select_double_clicks_into_a_box() {
    let mut d = Desk::new();
    d.click_text(p(1_000, 1_000));
    d.type_text(&"a\n".repeat(MAX_TEXT_LINES + 5));
    let e = d.mine()[0].clone();
    assert_eq!(e.text.split('\n').count(), MAX_TEXT_LINES);
    assert_eq!(d.editor.take_note(), Some(TOO_LONG));
    d.text_key(TextKey::Enter);
    assert_eq!(d.mine()[0].text, e.text, "no line past the last");
    d.text_key(TextKey::Finish);
    // Pasted, the lines past the last are cut.
    d.editor.paste(
        Some(&"b\n".repeat(80)),
        Some(p(30_000, 1_000)),
        &mut d.board,
        d.now,
    );
    assert_eq!(
        d.mine().last().unwrap().text.split('\n').count(),
        MAX_TEXT_LINES
    );
    // A double click with the pen draws; it doesn't edit.
    d.tool(DrawTool::Pen);
    let at = p(1_200, 1_500);
    d.stroke(&[at], false);
    d.stroke(&[at], false);
    d.editor.double_click(at, &mut d.board, d.now);
    assert!(!d.editor.typing());
    assert_eq!(d.editor.tool, DrawTool::Pen);
    // Ctrl+C with nothing selected leaves the clipboard alone.
    d.key(DrawAction::Leave);
    assert_eq!(d.editor.copy(&d.board), None);
}

#[test]
fn a_style_picked_while_typing_restyles_the_box() {
    let mut d = Desk::new();
    d.click_text(p(1_000, 1_000));
    d.type_text("big");
    let small = d.mine()[0].points[1];
    d.editor
        .set_style(Some(4), Some(2), None, &mut d.board, d.now);
    let e = d.mine()[0].clone();
    assert_eq!((e.colour, e.width), (4, 2));
    assert!(e.points[1].x > small.x && e.points[1].y > small.y, "larger");
    d.editor.step_fade(FadeStep::Next, &mut d.board, d.now);
    assert!(d.mine()[0].fades && !d.mine()[0].done);
    d.text_key(TextKey::Finish);
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
    // The eraser takes a box anywhere in it.
    d.tool(DrawTool::Eraser);
    let (a, b) = (e.points[0], e.points[1]);
    let mid = p(a.x / 2 + b.x / 2, a.y / 2 + b.y / 2);
    d.stroke(&[mid, mid], false);
    assert!(d.mine().is_empty());
}

// ---- Editing text as text fields do (the author's request, M27c) ----

#[test]
fn words_and_the_selection_move_as_in_text_fields() {
    let mut d = Desk::new();
    d.click_text(p(1_000, 1_000));
    d.type_text("Hello big world");
    // Ctrl+Left and Ctrl+Right by word.
    d.text_key_with(TextKey::Left, false, true);
    d.text_key_with(TextKey::Left, false, true);
    d.type_text("<");
    assert_eq!(d.text_now(), "Hello <big world");
    d.text_key(TextKey::Backspace);
    d.text_key_with(TextKey::Right, false, true);
    d.type_text(">");
    assert_eq!(d.text_now(), "Hello big >world");
    d.text_key(TextKey::Backspace);
    // Shift+Ctrl+Right selects the word; typing replaces it.
    d.text_key_with(TextKey::Right, true, true);
    assert_eq!(d.editor.copy_text().as_deref(), Some("world"));
    d.type_text("there");
    assert_eq!(d.text_now(), "Hello big there");
    // Ctrl+Backspace takes a word, then the spaces and the word before.
    d.text_key_with(TextKey::Backspace, false, true);
    assert_eq!(d.text_now(), "Hello big ");
    d.text_key_with(TextKey::Backspace, false, true);
    assert_eq!(d.text_now(), "Hello ");
    // Ctrl+Home, then Ctrl+Delete: a word and the spaces after it, then
    // punctuation and its spaces (as Windows' text fields).
    d.type_text("a, b");
    d.text_key_with(TextKey::Home, false, true);
    d.text_key_with(TextKey::Delete, false, true);
    assert_eq!(d.text_now(), "a, b");
    d.text_key_with(TextKey::End, false, true);
    d.text_key_with(TextKey::Left, false, true);
    d.text_key_with(TextKey::Left, false, true);
    d.text_key_with(TextKey::Delete, false, true);
    assert_eq!(d.text_now(), "ab");
    // Shift+Home selects to the line's start; Left collapses to its start.
    d.text_key_with(TextKey::End, false, false);
    d.text_key_with(TextKey::Home, true, false);
    assert_eq!(d.editor.copy_text().as_deref(), Some("ab"));
    d.text_key(TextKey::Left);
    assert_eq!(d.editor.copy_text(), None);
    d.type_text("[");
    assert_eq!(d.text_now(), "[ab");
    // Ctrl+A and Ctrl+X: all of it out, and the box with it.
    d.text_key(TextKey::SelectAll);
    assert_eq!(
        d.editor.cut_text(&mut d.board, d.now).as_deref(),
        Some("[ab")
    );
    assert!(d.mine().is_empty());
    d.type_text("again");
    d.text_key(TextKey::Finish);
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
}

#[test]
fn up_and_down_keep_the_place_across_and_selections_replace() {
    let mut d = Desk::new();
    let anchor = p(1_000, 1_000);
    d.click_text(anchor);
    d.type_text("abcdef\nab");
    // Up from the end of "ab": the same place across, after "ab".
    d.text_key(TextKey::Up);
    d.type_text("|");
    assert_eq!(d.text_now(), "ab|cdef\nab");
    d.text_key(TextKey::Backspace);
    // Down from the end of the long line: the short one's end.
    d.text_key(TextKey::End);
    d.text_key(TextKey::Down);
    d.type_text("!");
    assert_eq!(d.text_now(), "abcdef\nab!");
    // Down on the last line: its end; Up on the first: the start.
    d.text_key(TextKey::Up);
    d.text_key(TextKey::Up);
    d.type_text("^");
    assert_eq!(d.text_now(), "^abcdef\nab!");
    // Shift+Down selects across the line break; typing replaces it all.
    d.text_key_with(TextKey::Down, true, false);
    d.text_key_with(TextKey::End, true, false);
    assert_eq!(d.editor.copy_text().as_deref(), Some("abcdef\nab!"));
    d.type_text("one");
    assert_eq!(d.text_now(), "^one");
    d.text_key(TextKey::Finish);
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
}

#[test]
fn dragging_and_double_clicking_select_text() {
    let mut d = Desk::new();
    let anchor = p(1_000, 1_000);
    d.click_text(anchor);
    d.type_text("Go left now\nthen jump");
    // A drag inside the box selects, from where it began.
    let (from, to) = (d.over_char(anchor, 0, 3), d.over_char(anchor, 0, 7));
    d.editor.press(from, false, &mut d.board, d.now);
    d.editor.drag(to, false, &mut d.board, d.now);
    d.editor.release(Some(to), false, &mut d.board, d.now);
    assert_eq!(d.editor.copy_text().as_deref(), Some("left"));
    // Shift and a click extend it.
    let end = d.over_char(anchor, 1, 4);
    d.editor.press(end, true, &mut d.board, d.now);
    d.editor.release(Some(end), true, &mut d.board, d.now);
    assert_eq!(d.editor.copy_text().as_deref(), Some("left now\nthen"));
    // A double click selects the word there; a click alone none.
    let jump = d.over_char(anchor, 1, 6);
    d.editor.press(jump, false, &mut d.board, d.now);
    d.editor.double_click(jump, &mut d.board, d.now);
    d.editor.release(Some(jump), false, &mut d.board, d.now);
    assert_eq!(d.editor.copy_text().as_deref(), Some("jump"));
    // Painted: one box for the word, on the second line.
    let cursor = d.editor.text_cursor().unwrap();
    assert_eq!(cursor.selection.len(), 1);
    assert!(cursor.selection[0].0.y > anchor.y + 2_000);
    d.type_text("run");
    assert_eq!(d.text_now(), "Go left now\nthen run");
    d.text_key(TextKey::Finish);
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
}

#[test]
fn word_moves_stop_at_line_breaks_and_an_empty_line_selects_nothing() {
    let mut d = Desk::new();
    let anchor = p(1_000, 1_000);
    d.click_text(anchor);
    d.type_text("abc\n\n  def");
    let mark = |d: &mut Desk| {
        d.type_text("|");
        let text = d.text_now();
        d.text_key(TextKey::Backspace);
        text
    };
    // Ctrl+Right: the word's end, then past each break, then the indent.
    d.text_key_with(TextKey::Home, false, true);
    d.text_key_with(TextKey::Right, false, true);
    assert_eq!(mark(&mut d), "abc|\n\n  def");
    d.text_key_with(TextKey::Right, false, true);
    assert_eq!(mark(&mut d), "abc\n|\n  def");
    d.text_key_with(TextKey::Right, false, true);
    d.text_key_with(TextKey::Right, false, true);
    assert_eq!(mark(&mut d), "abc\n\n  |def");
    // Ctrl+Left: the line's start, then back over each break.
    d.text_key_with(TextKey::Left, false, true);
    assert_eq!(mark(&mut d), "abc\n\n|  def");
    d.text_key_with(TextKey::Left, false, true);
    assert_eq!(mark(&mut d), "abc\n|\n  def");
    // A double click on the empty line selects nothing; at a line's end,
    // the word before it.
    let empty = d.over_char(anchor, 1, 0);
    d.editor.press(empty, false, &mut d.board, d.now);
    d.editor.double_click(empty, &mut d.board, d.now);
    d.editor.release(Some(empty), false, &mut d.board, d.now);
    assert_eq!(d.editor.copy_text(), None);
    let end = d.over_char(anchor, 0, 3);
    d.editor.press(end, false, &mut d.board, d.now);
    d.editor.double_click(end, &mut d.board, d.now);
    d.editor.release(Some(end), false, &mut d.board, d.now);
    assert_eq!(d.editor.copy_text().as_deref(), Some("abc"));
    // Ctrl+Delete at a line's end takes the break alone.
    d.text_key(TextKey::End);
    d.text_key_with(TextKey::Delete, false, true);
    assert_eq!(d.text_now(), "abc\n  def");
    d.text_key(TextKey::Finish);
    d.send();
    assert_eq!(d.hosts_copy(), d.mine());
}
