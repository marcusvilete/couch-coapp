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
    board::{COLOUR_NAMES, ElementKind, PALETTE, Point, WIDTH_NAMES, WIDTHS, line_width},
    ids::MemberId,
};
use egui::{
    Align2, Color32, FontId, Margin, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::{
    board::{BoardView, LASER, PING, Shown},
    editor::{Editor, Keyed, bounds},
    hud::{BAR, BLUE, BLUE_TINT, BORDER, CONTROL_HOVER, FAINT, TEXT},
};

/// While something on the board moves by itself, the window paints at most
/// this often for it.
pub const MOVING_EVERY: Duration = Duration::from_millis(33);

/// Something picked on the toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Click {
    Tool(DrawTool),
    Colour(u8),
    Width(u8),
    Fades(bool),
    Undo,
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
    /// Who drew, for a ping's name.
    pub names: Vec<(MemberId, String)>,
    /// The main button is down for the editor.
    pub held: bool,
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
            for shown in self.board.elements() {
                for shape in element_shapes(shown, picture.height(), now, &place) {
                    painter.add(shape);
                }
            }
            if self.on
                && let Some(editor) = &self.editor
            {
                for id in editor.selection() {
                    if let Some(s) = self.board.get(editor.me(), *id) {
                        let (a, b) = bounds(&s.element);
                        let r = Rect::from_two_pos(place(a), place(b)).expand(6.0);
                        painter.rect_stroke(r, 3.0, Stroke::new(1.5, BLUE), StrokeKind::Outside);
                    }
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
            let state = editor.map(|e| (e.tool, e.colour, e.width, e.fades));
            let mut clicks = Vec::new();
            let shown = egui::Area::new(egui::Id::new("drawing"))
                .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -14.0))
                .show(ctx, |ui| {
                    if let Some(state) = state {
                        toolbar(ui, state, (size.x - 40.0).max(120.0), &mut clicks);
                    }
                });
            self.toolbar = Some(shown.response.rect);
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
                Click::Fades(f) => editor.set_style(None, None, Some(f), board, now),
                Click::Undo => {
                    editor.key(DrawAction::Undo, false, board, now);
                }
                Click::Redo => {
                    editor.key(DrawAction::Redo, false, board, now);
                }
                Click::Done => done = true,
            }
        }
        done
    }

    /// A key the board takes; `true` if drawing mode must end.
    pub fn key(&mut self, action: DrawAction, shift: bool, now: Instant) -> bool {
        match &mut self.editor {
            Some(editor) => editor.key(action, shift, &mut self.board, now) == Keyed::LeaveDrawing,
            None => false,
        }
    }

    /// Whether something on the board moves by itself.
    pub fn moving(&self, now: Instant) -> bool {
        !self.hidden && self.picture.is_some() && self.board.moving(now)
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
    }
    shapes
}

/// The toolbar: the tools; then the colours, widths, staying or fading,
/// undo and redo, and Done; and a line of hints. At most `max_width` wide:
/// its rows wrap in a narrow window.
fn toolbar(
    ui: &mut Ui,
    (tool, picked, width, fades): (DrawTool, u8, u8, bool),
    max_width: f32,
    clicks: &mut Vec<Click>,
) {
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
                for w in 0..WIDTHS as u8 {
                    let dot = 2.0 + 2.0 * f32::from(w);
                    let tip = WIDTH_NAMES[usize::from(w)];
                    if icon_button(ui, w == width, |p, r, c| {
                        p.circle_filled(r.center(), dot, c);
                    })
                    .on_hover_text(tip)
                    .clicked()
                    {
                        clicks.push(Click::Width(w));
                    }
                }
                ui.separator();
                let tip = if fades {
                    "Fades away after a few seconds (click: stays)"
                } else {
                    "Stays until erased (click: fades away)"
                };
                if icon_button(ui, fades, fade_icon)
                    .on_hover_text(tip)
                    .clicked()
                {
                    clicks.push(Click::Fades(!fades));
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
                if ui.add(done).on_hover_text("Back to the game · Esc").clicked() {
                    clicks.push(Click::Done);
                }
            });
            if max_width >= 480.0 {
                ui.label(
                    egui::RichText::new(
                        "Shift: straight lines, squares and circles · middle click: ping · Esc: back to the game",
                    )
                    .size(11.5)
                    .color(FAINT),
                );
            }
        });
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
