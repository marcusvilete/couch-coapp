//! Drawing on the stream in this window (M27a, ADR 0013): the board painted
//! over the picture, and in drawing mode the toolbar at the bottom centre.
//!
//! It keeps the HUD's rules (`crate::hud`): no popups, plain tooltips only,
//! and nothing animates by itself unless something on the board moves (a
//! fading element, a laser trail, a ping), and then at most
//! [`MOVING_EVERY`]. Its icons are painted, not taken from a font, so they
//! are alike on every system.

use std::time::{Duration, Instant};

use couch_input::shortcuts::{DrawAction, DrawTool, tool_key};
use couch_proto::{
    board::{
        COLOUR_NAMES, ElementKind, LINE_HEIGHT, PALETTE, Point, TEXT_SIZE_NAMES, WIDTH_NAMES,
        WIDTHS, line_width, text_size,
    },
    ids::MemberId,
};
use egui::{
    Align2, Color32, FontId, Margin, Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui,
    pos2, vec2,
};

use crate::{
    board::{BoardView, LASER, PING, Shown},
    editor::{Editor, FadeStep, Keyed, Measure, TextKey, bounds},
    hud::{BAR, BLUE, BLUE_TINT, BORDER, CONTROL_HOVER, FAINT, TEXT},
};

/// The size text is measured at: a text box's width scales from it.
const MEASURE_AT: f32 = 64.0;
/// The toolbar's handle, a tab on its top edge.
const HANDLE: egui::Vec2 = egui::Vec2::new(44.0, 16.0);

/// While something on the board moves by itself, the window paints at most
/// this often for it.
pub const MOVING_EVERY: Duration = Duration::from_millis(33);

/// Something picked on the toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Click {
    Tool(DrawTool),
    Colour(u8),
    Width(u8),
    Fade(FadeStep),
    Undo,
    /// The handle or the pill: the toolbar hidden or shown again.
    ToggleTools,
    Redo,
    Done,
}

/// The board in this window, and drawing on it.
#[derive(Default)]
pub struct Drawing {
    pub board: BoardView,
    /// Ours, once the main process has said who we are.
    pub editor: Option<Editor>,
    /// Drawing mode: the mouse and keys are the board's.
    pub on: bool,
    /// The drawings show (the user's own choice, for this window).
    pub hidden: bool,
    /// The picture's place in the window, in points, as last painted.
    pub picture: Option<Rect>,
    /// The toolbar's place, as last painted: presses there are its own.
    pub toolbar: Option<Rect>,
    /// The fade button's place, as last painted: the wheel over it steps
    /// how long drawings stay.
    pub fade_button: Option<Rect>,
    /// Who drew, for a ping's name.
    pub names: Vec<(MemberId, String)>,
    /// The main button is down for the editor.
    pub held: bool,
    /// Where the free pointer last was, in points: where pasted text goes.
    pub pointer: Option<Pos2>,
    /// What an input method is composing into the text box, not yet typed
    /// (M27c): shown at the caret, never sent.
    pub preedit: String,
    /// The editor measures text with this window's font: set at the first
    /// paint after it came (fonts exist only once egui has run).
    pub measures: bool,
    /// The toolbar is hidden to a small pill (its handle or Tab), for this
    /// window's life: what it covered can be drawn on and picked.
    pub tools_hidden: bool,
    /// The host's own window (M27d): no game to go back to.
    pub own: bool,
    /// Pictures on the board (M27e): theirs and ours, by id.
    pub pictures: crate::pictures::Pictures,
    /// Those shown, as egui textures.
    textures: std::collections::HashMap<couch_proto::board::PictureId, egui::TextureHandle>,
    clicks: Vec<Click>,
}

impl Drawing {
    /// A window position in the picture's own coordinates; `None` outside
    /// it, unless `clamp`.
    pub fn to_picture(&self, at: Pos2, clamp: bool) -> Option<Point> {
        let r = self.picture?;
        if !clamp && !r.contains(at) {
            return None;
        }
        let x = ((at.x - r.min.x) / r.width()).clamp(0.0, 1.0);
        let y = ((at.y - r.min.y) / r.height()).clamp(0.0, 1.0);
        Some(Point::new(
            (x * 65_535.0).round() as u16,
            (y * 65_535.0).round() as u16,
        ))
    }

    /// Paints the board (unless hidden) and, in drawing mode, the toolbar,
    /// in a window `size` points large.
    pub fn paint(&mut self, ctx: &egui::Context, now: Instant, size: egui::Vec2) {
        self.board.prune(now);
        self.toolbar = None;
        self.fade_button = None;
        if !self.measures
            && let Some(editor) = &mut self.editor
        {
            let ctx = ctx.clone();
            editor.set_measure(Measure::new(move |line| {
                ctx.fonts_mut(|f| {
                    f.layout_no_wrap(
                        line.to_owned(),
                        FontId::proportional(MEASURE_AT),
                        Color32::WHITE,
                    )
                    .size()
                    .x
                }) / MEASURE_AT
            }));
            self.measures = true;
        }
        let Some(picture) = self.picture else {
            return;
        };
        if !self.hidden {
            let painter = ctx.layer_painter(egui::LayerId::background());
            let painter = painter.with_clip_rect(picture);
            let place = |p: Point| {
                pos2(
                    picture.min.x + f32::from(p.x) / 65_535.0 * picture.width(),
                    picture.min.y + f32::from(p.y) / 65_535.0 * picture.height(),
                )
            };
            // Textures for the pictures shown, made as their pictures are
            // decoded; those no longer shown let go.
            let shown_ids = self.shown_pictures();
            self.pictures.sync(&shown_ids);
            self.textures.retain(|id, _| shown_ids.contains(id));
            for id in &shown_ids {
                if !self.textures.contains_key(id)
                    && let Some(image) = self.pictures.image(id)
                {
                    let colour = egui::ColorImage::from_rgba_unmultiplied(
                        [image.width as usize, image.height as usize],
                        &image.rgba,
                    );
                    let texture =
                        ctx.load_texture("board-picture", colour, egui::TextureOptions::LINEAR);
                    self.textures.insert(*id, texture);
                }
            }
            for shown in self.board.elements() {
                if shown.element.kind == ElementKind::Picture {
                    let texture = shown.element.picture.and_then(|id| self.textures.get(&id));
                    paint_picture(&painter, shown, texture, now, &place);
                    continue;
                }
                if shown.element.kind == ElementKind::Text {
                    paint_text(&painter, shown, picture.height(), now, &place);
                    continue;
                }
                for shape in element_shapes(shown, picture.height(), now, &place) {
                    painter.add(shape);
                }
            }
            if self.on
                && let Some(editor) = &self.editor
            {
                // The box being typed in: its frame, what an input method
                // composes, and the caret (still: nothing blinks).
                if let Some(cursor) = editor.text_cursor() {
                    let (a, b) = cursor.corners;
                    let frame = Rect::from_two_pos(place(a), place(b)).expand(5.0);
                    painter.rect_stroke(frame, 3.0, Stroke::new(1.0, BLUE), StrokeKind::Outside);
                    for (a, b) in &cursor.selection {
                        painter.rect_filled(
                            Rect::from_two_pos(place(*a), place(*b)),
                            0.0,
                            Color32::from_rgba_unmultiplied(74, 159, 240, 90),
                        );
                    }
                    let top = place(cursor.caret.0);
                    let em = cursor.size * picture.height();
                    let mut caret_x = top.x;
                    if !self.preedit.is_empty() {
                        let laid = painter.layout_no_wrap(
                            self.preedit.clone(),
                            FontId::proportional(em),
                            colour(cursor.colour),
                        );
                        let width = laid.size().x;
                        painter.galley(top, laid, colour(cursor.colour));
                        let under = top.y + em * LINE_HEIGHT * 0.95;
                        painter.line_segment(
                            [pos2(top.x, under), pos2(top.x + width, under)],
                            Stroke::new(1.5, colour(cursor.colour)),
                        );
                        caret_x += width;
                    }
                    let bottom = place(cursor.caret.1);
                    painter.line_segment(
                        [pos2(caret_x, top.y), pos2(caret_x, bottom.y)],
                        Stroke::new(2.0, colour(cursor.colour)),
                    );
                }
                for id in editor.selection() {
                    if let Some(s) = self.board.get(editor.me(), *id) {
                        let (a, b) = bounds(&s.element);
                        let r = Rect::from_two_pos(place(a), place(b)).expand(6.0);
                        painter.rect_stroke(r, 3.0, Stroke::new(1.5, BLUE), StrokeKind::Outside);
                    }
                }
                // The selected element's resize handles (M27e).
                for c in editor.handles(&self.board) {
                    let r = Rect::from_center_size(place(c), vec2(10.0, 10.0));
                    painter.rect_filled(r, 2.0, Color32::WHITE);
                    painter.rect_stroke(r, 2.0, Stroke::new(1.5, BLUE), StrokeKind::Middle);
                }
                if let Some((a, b)) = editor.marquee() {
                    let r = Rect::from_two_pos(place(a), place(b));
                    painter.rect(
                        r,
                        0.0,
                        Color32::from_rgba_unmultiplied(74, 159, 240, 30),
                        Stroke::new(1.0, BLUE),
                        StrokeKind::Inside,
                    );
                }
            }
            // The laser trails, newest brightest, each in its author's
            // colour.
            let laser_width = 0.006 * picture.height();
            for trail in self.board.lasers().values() {
                for pair in trail.windows(2) {
                    if pair[1].start {
                        continue;
                    }
                    let age = now.saturating_duration_since(pair[1].since).as_secs_f32();
                    let alpha = (1.0 - age / LASER.as_secs_f32()).clamp(0.0, 1.0);
                    let tint = with_alpha(colour(pair[1].colour), alpha);
                    let ends = [place(pair[0].at), place(pair[1].at)];
                    painter.line_segment(
                        ends,
                        Stroke::new(laser_width * 1.8, with_alpha(tint, alpha * 0.4)),
                    );
                    painter.line_segment(ends, Stroke::new(laser_width, tint));
                }
                if let Some(head) = trail.last() {
                    let age = now.saturating_duration_since(head.since).as_secs_f32();
                    let alpha = (1.0 - age / LASER.as_secs_f32()).clamp(0.0, 1.0);
                    painter.circle_filled(
                        place(head.at),
                        laser_width,
                        with_alpha(Color32::WHITE, alpha),
                    );
                }
            }
            // Pings: a ring pulsing, twice, with who.
            for ping in self.board.pings() {
                let age = now.saturating_duration_since(ping.since).as_secs_f32();
                let phase = (age * 2.0 / PING.as_secs_f32()).fract();
                let alpha = 1.0 - phase;
                let at = place(ping.at);
                let radius = picture.height() * (0.015 + 0.04 * phase);
                let ring = with_alpha(colour(2), alpha);
                painter.circle_stroke(at, radius, Stroke::new(3.0, ring));
                painter.circle_filled(at, 4.0, colour(2));
                let name = self
                    .names
                    .iter()
                    .find(|(m, _)| *m == ping.author)
                    .map_or("Someone", |(_, n)| n.as_str());
                painter.text(
                    at + vec2(0.0, picture.height() * 0.06),
                    Align2::CENTER_TOP,
                    name,
                    FontId::proportional(14.0),
                    Color32::WHITE,
                );
            }
        }
        if self.on {
            let editor = self.editor.as_ref();
            let state = editor.map(|e| (e.tool, e.colour, e.width, e.fades, e.stay));
            let mut clicks = Vec::new();
            let mut fade_button = None;
            let hidden = self.tools_hidden;
            let own = self.own;
            let shown = egui::Area::new(egui::Id::new(if hidden {
                "drawing-pill"
            } else {
                "drawing"
            }))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -14.0))
            .show(ctx, |ui| {
                if let Some(state) = state {
                    if hidden {
                        tools_pill(ui, state, &mut clicks);
                    } else {
                        toolbar(
                            ui,
                            state,
                            (size.x - 40.0).max(120.0),
                            own,
                            &mut clicks,
                            &mut fade_button,
                        );
                    }
                }
            });
            let mut rect = shown.response.rect;
            // The handle: a tab on the toolbar's top edge, centred, that
            // hides it (M27c, the author's request).
            if !hidden && state.is_some() {
                let tab = Rect::from_center_size(
                    pos2(rect.center().x, rect.top() - HANDLE.y / 2.0 + 1.0),
                    HANDLE,
                );
                egui::Area::new(egui::Id::new("drawing-handle"))
                    .fixed_pos(tab.min)
                    .show(ctx, |ui| {
                        let (r, response) = ui.allocate_exact_size(HANDLE, Sense::click());
                        let fill = if response.hovered() {
                            CONTROL_HOVER
                        } else {
                            BAR
                        };
                        ui.painter().rect(
                            r,
                            egui::CornerRadius {
                                nw: 6,
                                ne: 6,
                                sw: 0,
                                se: 0,
                            },
                            BAR,
                            Stroke::new(1.0, BORDER),
                            StrokeKind::Inside,
                        );
                        ui.painter().rect_filled(r.shrink(1.0), 5.0, fill);
                        chevron(ui.painter(), r.center(), false, TEXT);
                        if response
                            .on_hover_text("Hide the drawing tools · Tab")
                            .clicked()
                        {
                            clicks.push(Click::ToggleTools);
                        }
                    });
                rect = rect.union(tab);
            }
            self.toolbar = Some(rect);
            self.fade_button = fade_button;
            self.clicks = clicks;
        }
    }

    /// What the toolbar was asked; `true` if drawing mode must end.
    pub fn apply_clicks(&mut self, now: Instant) -> bool {
        let Some(editor) = &mut self.editor else {
            return false;
        };
        let mut done = false;
        for click in std::mem::take(&mut self.clicks) {
            let board = &mut self.board;
            match click {
                Click::Tool(tool) => editor.set_tool(tool, board, now),
                Click::Colour(c) => editor.set_style(Some(c), None, None, board, now),
                Click::Width(w) => editor.set_style(None, Some(w), None, board, now),
                Click::Fade(step) => editor.step_fade(step, board, now),
                Click::Undo => {
                    editor.key(DrawAction::Undo, false, board, now);
                }
                Click::Redo => {
                    editor.key(DrawAction::Redo, false, board, now);
                }
                Click::Done => done = true,
                Click::ToggleTools => self.tools_hidden = !self.tools_hidden,
            }
        }
        done
    }

    /// A key the board takes; `true` if drawing mode must end.
    /// The mouse wheel at `at` (the HUD's points), `up` away from the user:
    /// over the fade button it steps how long drawings stay. Whether it did.
    pub fn wheel(&mut self, at: Pos2, up: bool, now: Instant) -> bool {
        let over = self.fade_button.is_some_and(|r| r.contains(at));
        match &mut self.editor {
            Some(editor) if over => {
                let step = if up {
                    FadeStep::Longer
                } else {
                    FadeStep::Shorter
                };
                editor.step_fade(step, &mut self.board, now);
                true
            }
            _ => false,
        }
    }

    pub fn key(&mut self, action: DrawAction, shift: bool, now: Instant) -> bool {
        match &mut self.editor {
            Some(editor) => match editor.key(action, shift, &mut self.board, now) {
                Keyed::LeaveDrawing => true,
                Keyed::ToggleToolbar => {
                    self.tools_hidden = !self.tools_hidden;
                    false
                }
                Keyed::Done => false,
            },
            None => false,
        }
    }

    /// A text box is being typed in: keys and text are its.
    pub fn typing(&self) -> bool {
        self.editor.as_ref().is_some_and(Editor::typing)
    }

    /// Text typed into the box (SDL's text input: accents, dead keys and
    /// input methods composed); what was being composed is done.
    pub fn type_text(&mut self, text: &str, now: Instant) {
        self.preedit.clear();
        if let Some(editor) = &mut self.editor {
            editor.type_text(text, &mut self.board, now);
        }
    }

    pub fn text_key(&mut self, key: TextKey, shift: bool, word: bool, now: Instant) {
        if let Some(editor) = &mut self.editor {
            editor.text_key(key, shift, word, &mut self.board, now);
        }
        if !self.typing() {
            self.preedit.clear();
        }
    }

    /// Ctrl+C or Ctrl+X in the text box: its selected text, for the system
    /// clipboard (taken out for Ctrl+X).
    pub fn copy_text(&mut self, cut: bool, now: Instant) -> Option<String> {
        let editor = self.editor.as_mut()?;
        if cut {
            editor.cut_text(&mut self.board, now)
        } else {
            editor.copy_text()
        }
    }

    /// Ctrl+C: what goes on the system clipboard.
    pub fn copy(&mut self) -> Option<String> {
        self.editor.as_mut().and_then(|e| e.copy(&self.board))
    }

    /// The pictures the board shows, by id (M27e).
    pub fn shown_pictures(&self) -> std::collections::BTreeSet<couch_proto::board::PictureId> {
        self.board
            .elements()
            .iter()
            .filter_map(|s| s.element.picture)
            .collect()
    }

    /// A picture made from the clipboard or a file, pasted as ours (M27e):
    /// kept, so it goes to the host with its bytes, and shown at once.
    pub fn paste_picture(&mut self, ready: crate::pictures::Ready, now: Instant) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let p = ready.prepared;
        let shown = self
            .board
            .elements()
            .iter()
            .filter_map(|s| s.element.picture)
            .collect();
        self.pictures.add(p.id, p.bytes, &shown);
        editor.paste_picture(p.id, p.width, p.height, ready.at, &mut self.board, now);
    }

    /// Ctrl+V, with the system clipboard's text, at the pointer if over the
    /// picture.
    pub fn paste(&mut self, system: Option<&str>, at: Option<Point>, now: Instant) {
        if let Some(editor) = &mut self.editor {
            editor.paste(system, at, &mut self.board, now);
        }
    }

    /// The box being typed in, in the window's points: where an input
    /// method's candidates go. Its top-left, and the caret's top and
    /// bottom.
    pub fn text_area(&self) -> Option<(Pos2, Pos2, Pos2)> {
        let picture = self.picture?;
        let cursor = self.editor.as_ref()?.text_cursor()?;
        let place = |p: Point| {
            pos2(
                picture.min.x + f32::from(p.x) / 65_535.0 * picture.width(),
                picture.min.y + f32::from(p.y) / 65_535.0 * picture.height(),
            )
        };
        Some((
            place(cursor.corners.0),
            place(cursor.caret.0),
            place(cursor.caret.1),
        ))
    }

    /// Whether something on the board moves by itself.
    pub fn moving(&self, now: Instant) -> bool {
        !self.hidden && self.picture.is_some() && self.board.moving(now)
    }

    /// Whether a fading element has started to fade since the board was
    /// last painted at `painted`: nothing moved while it stayed, and a
    /// still picture brings no frames (the author's run of M27b).
    pub fn fade_due(&self, painted: Option<Instant>, now: Instant) -> bool {
        !self.hidden
            && self.picture.is_some()
            && painted
                .and_then(|p| self.board.next_fade(p))
                .is_some_and(|at| at <= now)
    }
}

fn colour(index: u8) -> Color32 {
    let [r, g, b] = PALETTE[usize::from(index).min(PALETTE.len() - 1)];
    Color32::from_rgb(r, g, b)
}

fn with_alpha(c: Color32, alpha: f32) -> Color32 {
    let a = (alpha.clamp(0.0, 1.0) * f32::from(c.a())).round() as u8;
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// One element's shapes on the picture, `height` points tall.
fn element_shapes(
    shown: &Shown,
    height: f32,
    now: Instant,
    place: &impl Fn(Point) -> Pos2,
) -> Vec<Shape> {
    let e = &shown.element;
    let alpha = shown.alpha(now);
    if alpha <= 0.0 || e.points.is_empty() {
        return Vec::new();
    }
    let see_through = if e.kind == ElementKind::Highlighter {
        0.38
    } else {
        1.0
    };
    let c = with_alpha(colour(e.colour), alpha * see_through);
    let width = (line_width(e.kind, e.width) * height).max(1.0);
    let stroke = Stroke::new(width, c);
    let points: Vec<Pos2> = e.points.iter().map(|p| place(*p)).collect();
    let mut shapes = Vec::new();
    match e.kind {
        ElementKind::Pen | ElementKind::Highlighter => {
            if points.len() == 1 {
                shapes.push(Shape::circle_filled(points[0], width / 2.0, c));
            } else {
                // Round ends, and joins that don't show gaps at sharp turns.
                if e.kind == ElementKind::Pen {
                    for p in [points[0], points[points.len() - 1]] {
                        shapes.push(Shape::circle_filled(p, width / 2.0, c));
                    }
                }
                shapes.push(Shape::line(points, stroke));
            }
        }
        ElementKind::Line => shapes.push(Shape::line_segment([points[0], points[1]], stroke)),
        ElementKind::Arrow => {
            let (a, b) = (points[0], points[1]);
            shapes.push(Shape::line_segment([a, b], stroke));
            let along = b - a;
            let len = along.length();
            if len > 1.0 {
                let dir = along / len;
                let head = (width * 4.0).max(10.0).min(len * 0.5);
                let side = vec2(-dir.y, dir.x);
                for s in [1.0, -1.0] {
                    let tip = b - dir * head + side * (head * 0.55 * s);
                    shapes.push(Shape::line_segment([b, tip], stroke));
                }
            }
        }
        ElementKind::Rectangle => {
            let r = Rect::from_two_pos(points[0], points[1]);
            shapes.push(Shape::rect_stroke(r, 2.0, stroke, StrokeKind::Middle));
        }
        ElementKind::Ellipse => {
            let r = Rect::from_two_pos(points[0], points[1]);
            shapes.push(Shape::ellipse_stroke(r.center(), r.size() / 2.0, stroke));
        }
        // Laid out with the fonts: `paint_text`; a picture with its
        // texture: `paint_picture`.
        ElementKind::Text | ElementKind::Picture => {}
    }
    shapes
}

/// A picture on the board (M27e), stretched over its box, fading as any
/// element; while its bytes are on their way or being decoded, a faint
/// frame where it goes. The board overlay paints it alike (`paint.ts`).
fn paint_picture(
    painter: &Painter,
    shown: &Shown,
    texture: Option<&egui::TextureHandle>,
    now: Instant,
    place: &impl Fn(Point) -> Pos2,
) {
    let alpha = shown.alpha(now);
    let e = &shown.element;
    if alpha <= 0.0 || e.points.len() != 2 {
        return;
    }
    let r = Rect::from_two_pos(place(e.points[0]), place(e.points[1]));
    match texture {
        Some(t) => {
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            painter.image(
                t.id(),
                r,
                uv,
                Color32::from_white_alpha((alpha * 255.0) as u8),
            );
        }
        None => {
            let faint = Color32::from_white_alpha((alpha * 90.0) as u8);
            painter.rect_stroke(r, 2.0, Stroke::new(1.0, faint), StrokeKind::Inside);
        }
    }
}

/// A text box on the picture, `height` points tall: each line from its
/// first point down, a line height apart, over a soft shadow so it reads
/// over any game. The board overlay paints it alike (`paint.ts`).
fn paint_text(
    painter: &Painter,
    shown: &Shown,
    height: f32,
    now: Instant,
    place: &impl Fn(Point) -> Pos2,
) {
    let e = &shown.element;
    let alpha = shown.alpha(now);
    let Some(corner) = e.points.first() else {
        return;
    };
    if alpha <= 0.0 {
        return;
    }
    let em = text_size(e.width) * height;
    let font = FontId::proportional(em);
    let shadow = (em * 0.06).max(1.0);
    let top = place(*corner);
    for (i, line) in e.text.split('\n').enumerate() {
        let at = pos2(top.x, top.y + i as f32 * LINE_HEIGHT * em);
        painter.text(
            at + vec2(shadow, shadow),
            Align2::LEFT_TOP,
            line,
            font.clone(),
            with_alpha(Color32::BLACK, alpha * 0.6),
        );
        painter.text(
            at,
            Align2::LEFT_TOP,
            line,
            font.clone(),
            with_alpha(colour(e.colour), alpha),
        );
    }
}

/// The toolbar: the tools; then the colours, widths, staying or fading,
/// undo and redo, and Done; and a line of hints. At most `max_width` wide:
/// its rows wrap in a narrow window.
fn toolbar(
    ui: &mut Ui,
    (tool, picked, width, fades, stay): (DrawTool, u8, u8, bool, u8),
    max_width: f32,
    own: bool,
    clicks: &mut Vec<Click>,
    fade_button: &mut Option<Rect>,
) {
    // The host's own window (M27d) has no game to go back to.
    let back = if own { "done" } else { "back to the game" };
    egui::Frame::new()
        .fill(BAR)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_max_width(max_width);
            ui.spacing_mut().item_spacing = vec2(4.0, 6.0);
            ui.horizontal_wrapped(|ui| {
                for t in DrawTool::ALL {
                    let tip = format!("{} · {}", t.name(), tool_key(t));
                    if icon_button(ui, t == tool, |p, r, c| tool_icon(p, r, t, c, picked))
                        .on_hover_text(tip)
                        .clicked()
                    {
                        clicks.push(Click::Tool(t));
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (i, name) in COLOUR_NAMES.iter().enumerate() {
                    let i = i as u8;
                    let (rect, response) = ui.allocate_exact_size(vec2(24.0, 30.0), Sense::click());
                    let ring = if i == picked { TEXT } else { Color32::TRANSPARENT };
                    ui.painter().circle_filled(rect.center(), 8.0, colour(i));
                    ui.painter()
                        .circle_stroke(rect.center(), 11.0, Stroke::new(2.0, ring));
                    if response.on_hover_text(*name).clicked() {
                        clicks.push(Click::Colour(i));
                    }
                }
                ui.separator();
                // Widths, or with the Text tool the text's sizes.
                let text = tool == DrawTool::Text;
                for w in 0..WIDTHS as u8 {
                    let dot = 2.0 + 2.0 * f32::from(w);
                    let tip = if text {
                        TEXT_SIZE_NAMES[usize::from(w)]
                    } else {
                        WIDTH_NAMES[usize::from(w)]
                    };
                    if icon_button(ui, w == width, |p, r, c| {
                        if text {
                            p.text(
                                r.center(),
                                Align2::CENTER_CENTER,
                                "A",
                                FontId::proportional(10.0 + 4.0 * f32::from(w)),
                                c,
                            );
                        } else {
                            p.circle_filled(r.center(), dot, c);
                        }
                    })
                    .on_hover_text(tip)
                    .clicked()
                    {
                        clicks.push(Click::Width(w));
                    }
                }
                ui.separator();
                // Staying or fading, and how long: one button (the author's
                // choice, M27b). A click goes round stays, 2, 4, 8, 15 s; the
                // wheel over it steps up or down (`Drawing::wheel`).
                let tip = if fades {
                    format!(
                        "Fades away after {stay} seconds · click: longer, then stays · wheel: longer or shorter"
                    )
                } else {
                    "Stays until erased · click: fades away · wheel: longer or shorter".to_string()
                };
                let (rect, response) = ui.allocate_exact_size(vec2(56.0, 30.0), Sense::click());
                let fill = if fades {
                    BLUE_TINT
                } else if response.hovered() {
                    CONTROL_HOVER
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, 6.0, fill);
                let c = if fades { BLUE } else { TEXT };
                if fades {
                    let icon = Rect::from_min_size(rect.min + vec2(5.0, 6.0), vec2(18.0, 18.0));
                    fade_icon(ui.painter(), icon, c);
                    ui.painter().text(
                        pos2(rect.max.x - 6.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        format!("{stay}s"),
                        FontId::proportional(12.0),
                        c,
                    );
                } else {
                    let icon = Rect::from_center_size(rect.center(), vec2(18.0, 18.0));
                    fade_icon(ui.painter(), icon, c);
                }
                *fade_button = Some(rect);
                if response.on_hover_text(tip).clicked() {
                    clicks.push(Click::Fade(FadeStep::Next));
                }
                ui.separator();
                if icon_button(ui, false, |p, r, c| turn_icon(p, r, c, false))
                    .on_hover_text("Undo · Ctrl+Z")
                    .clicked()
                {
                    clicks.push(Click::Undo);
                }
                if icon_button(ui, false, |p, r, c| turn_icon(p, r, c, true))
                    .on_hover_text("Redo · Ctrl+Shift+Z")
                    .clicked()
                {
                    clicks.push(Click::Redo);
                }
                let done = egui::Button::new(egui::RichText::new("Done").color(TEXT))
                    .fill(BLUE_TINT)
                    .min_size(vec2(0.0, 28.0));
                let tip = if own {
                    "Stop drawing · Esc"
                } else {
                    "Back to the game · Esc"
                };
                if ui.add(done).on_hover_text(tip).clicked() {
                    clicks.push(Click::Done);
                }
            });
            if max_width >= 480.0 {
                let hint = if tool == DrawTool::Text {
                    "Click to write, or on your text to change it · double click: a word · Esc: done"
                        .to_string()
                } else {
                    format!(
                        "Shift: straight lines, squares and circles · middle click: ping · Tab: hide the tools · Esc: {back}"
                    )
                };
                ui.label(
                    egui::RichText::new(hint)
                    .size(11.5)
                    .color(FAINT),
                );
            }
        });
}

/// The toolbar hidden: a small pill at the bottom centre showing the tool
/// and colour picked; a click on it shows the toolbar again.
fn tools_pill(
    ui: &mut Ui,
    (tool, picked, ..): (DrawTool, u8, u8, bool, u8),
    clicks: &mut Vec<Click>,
) {
    egui::Frame::new()
        .fill(BAR)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(15)
        .inner_margin(Margin::ZERO)
        .show(ui, |ui| {
            let (rect, response) = ui.allocate_exact_size(vec2(96.0, 32.0), Sense::click());
            if response.hovered() {
                ui.painter().rect_filled(rect, 12.0, CONTROL_HOVER);
            }
            let p = ui.painter();
            chevron(p, pos2(rect.min.x + 13.0, rect.center().y), true, TEXT);
            let icon =
                Rect::from_center_size(pos2(rect.center().x, rect.center().y), vec2(18.0, 18.0));
            tool_icon(p, icon, tool, TEXT, picked);
            p.circle_filled(
                pos2(rect.max.x - 13.0, rect.center().y),
                7.0,
                colour(picked),
            );
            let tip = format!("{} · show the drawing tools · Tab", tool.name());
            if response.on_hover_text(tip).clicked() {
                clicks.push(Click::ToggleTools);
            }
        });
}

/// A chevron at `at`, pointing up or down.
fn chevron(p: &egui::Painter, at: Pos2, up: bool, c: Color32) {
    let dy = if up { 2.5 } else { -2.5 };
    let s = Stroke::new(1.8, c);
    p.line_segment([at + vec2(-5.0, dy), at + vec2(0.0, -dy)], s);
    p.line_segment([at + vec2(0.0, -dy), at + vec2(5.0, dy)], s);
}

/// A 30-point square button with a painted icon, blue while `on`.
pub(crate) fn icon_button(
    ui: &mut Ui,
    on: bool,
    paint: impl FnOnce(&egui::Painter, Rect, Color32),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
    let fill = if on {
        BLUE_TINT
    } else if response.hovered() {
        CONTROL_HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 6.0, fill);
    let c = if on { BLUE } else { TEXT };
    paint(ui.painter(), rect.shrink(6.0), c);
    response
}

/// A point within `r`, by fractions of its size.
fn at(r: Rect, x: f32, y: f32) -> Pos2 {
    pos2(r.min.x + x * r.width(), r.min.y + y * r.height())
}

/// `picked`: the colour chosen, which the laser's icon shows.
fn tool_icon(p: &egui::Painter, r: Rect, tool: DrawTool, c: Color32, picked: u8) {
    let s = Stroke::new(1.6, c);
    match tool {
        DrawTool::Select => {
            let pts = [
                at(r, 0.25, 0.1),
                at(r, 0.25, 0.85),
                at(r, 0.45, 0.66),
                at(r, 0.6, 0.95),
                at(r, 0.72, 0.89),
                at(r, 0.58, 0.6),
                at(r, 0.85, 0.6),
            ];
            p.add(Shape::closed_line(pts.to_vec(), s));
        }
        DrawTool::Pen => pen_icon(p, r, c),
        DrawTool::Highlighter => {
            p.line_segment(
                [at(r, 0.2, 0.8), at(r, 0.8, 0.2)],
                Stroke::new(6.0, with_alpha(c, 0.5)),
            );
            p.line_segment([at(r, 0.2, 0.8), at(r, 0.8, 0.2)], Stroke::new(1.2, c));
        }
        // In the colour picked: the laser draws in it.
        DrawTool::Laser => {
            p.line_segment(
                [at(r, 0.15, 0.85), at(r, 0.65, 0.35)],
                Stroke::new(2.0, with_alpha(colour(picked), 0.5)),
            );
            p.circle_filled(at(r, 0.7, 0.3), 3.5, colour(picked));
        }
        DrawTool::Arrow => {
            let (a, b) = (at(r, 0.15, 0.85), at(r, 0.85, 0.15));
            p.line_segment([a, b], s);
            p.line_segment([b, at(r, 0.45, 0.15)], s);
            p.line_segment([b, at(r, 0.85, 0.55)], s);
        }
        DrawTool::Line => {
            p.line_segment([at(r, 0.15, 0.85), at(r, 0.85, 0.15)], s);
        }
        DrawTool::Rectangle => {
            p.rect_stroke(
                Rect::from_two_pos(at(r, 0.1, 0.22), at(r, 0.9, 0.78)),
                2.0,
                s,
                StrokeKind::Middle,
            );
        }
        DrawTool::Ellipse => {
            p.add(Shape::ellipse_stroke(
                r.center(),
                vec2(r.width() * 0.42, r.height() * 0.3),
                s,
            ));
        }
        // A T, as a letter of lines.
        DrawTool::Text => {
            p.line_segment([at(r, 0.18, 0.18), at(r, 0.82, 0.18)], s);
            p.line_segment([at(r, 0.5, 0.18), at(r, 0.5, 0.85)], s);
            p.line_segment([at(r, 0.38, 0.85), at(r, 0.62, 0.85)], s);
        }
        DrawTool::Eraser => {
            let pts = [
                at(r, 0.15, 0.6),
                at(r, 0.5, 0.2),
                at(r, 0.85, 0.5),
                at(r, 0.55, 0.85),
                at(r, 0.35, 0.85),
            ];
            p.add(Shape::closed_line(pts.to_vec(), s));
            p.line_segment([at(r, 0.32, 0.4), at(r, 0.68, 0.72)], s);
        }
    }
}

/// A pen, for the Pen tool and the bar's drawing button.
pub(crate) fn pen_icon(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.6, c);
    let pts = [
        at(r, 0.68, 0.12),
        at(r, 0.88, 0.32),
        at(r, 0.32, 0.88),
        at(r, 0.1, 0.9),
        at(r, 0.12, 0.68),
    ];
    p.add(Shape::closed_line(pts.to_vec(), s));
    p.line_segment([at(r, 0.58, 0.22), at(r, 0.78, 0.42)], s);
}

/// An eye, struck through while the drawings are hidden: the bar's button.
pub(crate) fn eye_icon(p: &egui::Painter, r: Rect, c: Color32, hidden: bool) {
    let s = Stroke::new(1.6, c);
    p.add(Shape::ellipse_stroke(
        r.center(),
        vec2(r.width() * 0.45, r.height() * 0.28),
        s,
    ));
    p.circle_filled(r.center(), r.height() * 0.13, c);
    if hidden {
        p.line_segment([at(r, 0.1, 0.9), at(r, 0.9, 0.1)], Stroke::new(2.0, c));
    }
}

/// A line fading toward its end: fades or stays.
fn fade_icon(p: &egui::Painter, r: Rect, c: Color32) {
    let steps = 5;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let a = at(r, 0.1 + 0.8 * t0, 0.5);
        let b = at(r, 0.1 + 0.8 * t1, 0.5);
        p.line_segment([a, b], Stroke::new(3.0, with_alpha(c, 1.0 - t0 * 0.85)));
    }
}

/// A curved arrow, back for undo, forward for redo.
fn turn_icon(p: &egui::Painter, r: Rect, c: Color32, forward: bool) {
    let s = Stroke::new(1.6, c);
    let flip = |x: f32| if forward { 1.0 - x } else { x };
    let arc: Vec<Pos2> = (0..=8)
        .map(|i| {
            let t = std::f32::consts::PI * (i as f32 / 8.0);
            at(r, flip(0.5 - 0.32 * t.cos()), 0.62 - 0.32 * t.sin())
        })
        .collect();
    let start = arc[0];
    p.add(Shape::line(arc, s));
    p.line_segment([start, at(r, flip(0.05), 0.48)], s);
    p.line_segment([start, at(r, flip(0.32), 0.48)], s);
}
