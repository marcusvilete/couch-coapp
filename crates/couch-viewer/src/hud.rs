//! The in-stream HUD, drawn with egui on top of the video (its look since
//! M21, the main window's dark palette):
//! - the session bar, in rows of fixed height so it never jumps: what is
//!   streaming and the window's controls; a chip per seat, saying who has
//!   it, to ask for one; where this viewer stands, with a note such as "the
//!   host said no"; the shortcuts;
//! - the centre card, when there is no picture to show, and why;
//! - the stats card (latency, path, decoder), bottom left.
//!
//! The bar takes clicks only while the mouse is free (not captured for the
//! game), and a click on it never reaches the game: the window loop hands
//! presses inside the bar here instead of to the input capture. So:
//! - **no widget that opens a popup** (combo boxes, menus, context menus)
//!   and no editable value boxes: a popup outside the bar would take neither
//!   clicks nor its closing. Plain tooltips only, never clickable;
//! - **the mouse being captured clears the HUD's pointer** (`pointer_gone`),
//!   so no hover or tooltip stays over the game;
//! - releases always come here once their press did (`pressed`), which also
//!   holds the bar up: a slider dragged outside the bar stays safe;
//! - the frame's shadow stays small: a click on it reaches the game, so the
//!   bar must look the size it acts.
//!
//! The bar shows for a while after anything changes, while the pointer moves
//! over the window, and while it rests on the bar (at most
//! [`HOLD_UNDER_POINTER`]), then gets out of the way. Nothing animates every
//! frame: the window presents in Mailbox mode, so that would spin the GPU.

use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use couch_input::shortcuts::{Shortcut, Shortcuts};
use couch_proto::{
    ipc::{HudRole, HudSeat, HudState, StreamVolume, UserAction},
    session::{EndReason, StreamState, seat_phrase},
};
use egui::{
    Align, Align2, Color32, CornerRadius, FontFamily, FontId, Layout, Margin, Pos2, Rect, RichText,
    Sense, Shadow, Stroke, StrokeKind, Ui,
    epaint::text::VariationCoords,
    pos2,
    text::{LayoutJob, TextFormat, TextWrapping},
    vec2,
};

/// How long the bar stays after a change, and a note with it.
const SHOW_AFTER_CHANGE: Duration = Duration::from_secs(5);
/// How long the bar stays after the pointer last moved.
const SHOW_AFTER_POINTER: Duration = Duration::from_secs(3);
/// How long the bar stays while the pointer rests on it.
const HOLD_UNDER_POINTER: Duration = Duration::from_secs(15);

/// Something the user asked for on the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudAction {
    Session(UserAction),
    Fullscreen,
    /// The slider or the mute button (P6).
    Volume(StreamVolume),
}

pub struct Hud {
    ctx: egui::Context,
    renderer: egui_wgpu::Renderer,
    started: Instant,
    /// Whether an icon font loaded; without one, icon buttons are words.
    icons: bool,
    /// The whole HUD (Ctrl+Alt+Shift+H by default).
    pub visible: bool,
    pub show_stats: bool,
    /// Full screen now, for the button's icon.
    pub fullscreen: bool,
    /// No picture has arrived yet.
    pub waiting: bool,
    /// A made-up session (`--hud-preview`): the bar stays up, over a test
    /// picture.
    pub preview: bool,
    /// The keys the bar's hint names.
    pub shortcuts: Shortcuts,
    /// The stream volume the bar shows (P6).
    pub volume: StreamVolume,
    /// While this viewer holds keyboard and mouse: how to capture the mouse
    /// or free it.
    pub mouse_hint: Option<String>,
    session: Option<HudState>,
    note: Option<(String, Instant)>,
    changed_at: Option<Instant>,
    pointer_at: Option<Instant>,
    /// Since when the pointer has rested on the bar.
    on_bar_since: Option<Instant>,
    events: Vec<egui::Event>,
    /// Where the bar was last drawn, in points.
    bar_rect: Option<Rect>,
    /// Buttons pressed on the bar: their releases come here too.
    pressed: BTreeSet<u8>,
    actions: Vec<HudAction>,
    /// Whether the bar was up at the last paint, so a timeout repaints.
    bar_painted: bool,
    /// When egui wants to paint again: it lays a new area out unseen on its
    /// first frame, and shows tooltips after a delay. Video repaints anyway;
    /// a still screen (no frame yet, a paused game) needs this.
    repaint_at: Option<Instant>,
}

impl Hud {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let ctx = egui::Context::default();
        let (fonts, icons) = fonts();
        ctx.set_fonts(fonts);
        set_style(&ctx);
        Self {
            ctx,
            renderer: egui_wgpu::Renderer::new(
                device,
                format,
                egui_wgpu::RendererOptions::default(),
            ),
            started: Instant::now(),
            icons,
            visible: true,
            show_stats: true,
            fullscreen: false,
            waiting: false,
            preview: false,
            shortcuts: Shortcuts::default(),
            volume: StreamVolume::default(),
            mouse_hint: None,
            session: None,
            note: None,
            changed_at: None,
            pointer_at: None,
            on_bar_since: None,
            events: Vec::new(),
            bar_rect: None,
            pressed: BTreeSet::new(),
            actions: Vec::new(),
            bar_painted: false,
            repaint_at: None,
        }
    }

    /// The session as the main process describes it.
    pub fn set_session(&mut self, state: HudState) {
        let now = Instant::now();
        let old = self.session.as_ref();
        if !state.note.is_empty() && old.is_none_or(|o| o.note != state.note) {
            self.note = Some((state.note.clone(), now + SHOW_AFTER_CHANGE));
        }
        let changed = old.is_none_or(|o| {
            (
                o.role,
                &o.seats,
                o.control_paused,
                o.game_in_front,
                o.stream,
            ) != (
                state.role,
                &state.seats,
                state.control_paused,
                state.game_in_front,
                state.stream,
            )
        });
        if changed {
            self.changed_at = Some(now);
        }
        self.session = Some(state);
    }

    pub fn session(&self) -> Option<&HudState> {
        self.session.as_ref()
    }

    /// Shows `note` for a while, as if the main process had sent it.
    pub fn note(&mut self, note: impl Into<String>) {
        let now = Instant::now();
        self.note = Some((note.into(), now + SHOW_AFTER_CHANGE));
        self.changed_at = Some(now);
    }

    /// The free pointer moved to `pos` (in points).
    pub fn pointer_moved(&mut self, pos: Pos2) {
        let now = Instant::now();
        self.pointer_at = Some(now);
        if self.bar_rect.is_some_and(|r| r.contains(pos)) {
            self.on_bar_since.get_or_insert(now);
        } else {
            self.on_bar_since = None;
        }
        self.events.push(egui::Event::PointerMoved(pos));
    }

    /// The pointer left the window, or the mouse was captured for the game:
    /// no hover or tooltip may stay.
    pub fn pointer_gone(&mut self) {
        self.on_bar_since = None;
        self.events.push(egui::Event::PointerGone);
    }

    /// The window lost the focus: a release that began on the bar may never
    /// come, and a held press would keep the bar up for good.
    pub fn focus_lost(&mut self) {
        self.pressed.clear();
        self.pointer_gone();
    }

    /// A free pointer's button; `true` if the HUD took it, in which case it
    /// must not go anywhere else.
    pub fn pointer_button(&mut self, pos: Pos2, button: u8, pressed: bool) -> bool {
        let taken = if pressed {
            self.bar_up(Instant::now()) && self.bar_rect.is_some_and(|r| r.contains(pos))
        } else {
            self.pressed.contains(&button)
        };
        if !taken {
            return false;
        }
        if pressed {
            self.pressed.insert(button);
        } else {
            self.pressed.remove(&button);
        }
        let button = match button {
            couch_proto::input::mouse_button::RIGHT => egui::PointerButton::Secondary,
            couch_proto::input::mouse_button::MIDDLE => egui::PointerButton::Middle,
            _ => egui::PointerButton::Primary,
        };
        self.events.push(egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
        true
    }

    /// What the user clicked since the last call.
    pub fn take_actions(&mut self) -> Vec<HudAction> {
        std::mem::take(&mut self.actions)
    }

    /// Whether the stats card shows, so its rows are worth building.
    pub fn wants_stats(&self) -> bool {
        self.visible && self.show_stats
    }

    /// Something changed that only a repaint shows.
    pub fn needs_paint(&self) -> bool {
        let now = Instant::now();
        !self.events.is_empty()
            || self.bar_up(now) != self.bar_painted
            || self.repaint_at.is_some_and(|t| now >= t)
    }

    fn bar_up(&self, now: Instant) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let recent = |at: Option<Instant>, within| at.is_some_and(|t| now - t < within);
        // Up while asking (unless playing meanwhile, asking to move: the bar
        // would cover the game), and while playing with the game out of
        // front on the host: the player must know why input does nothing.
        let playing = session.seats.iter().any(|seat| seat.mine);
        let asking = session.role == HudRole::Asking && !playing;
        let away = playing && !session.game_in_front;
        self.visible
            && (self.preview
                || asking
                || away
                || recent(self.changed_at, SHOW_AFTER_CHANGE)
                || recent(self.pointer_at, SHOW_AFTER_POINTER)
                || recent(self.on_bar_since, HOLD_UNDER_POINTER)
                || self.note.as_ref().is_some_and(|(_, until)| now < *until)
                || !self.pressed.is_empty())
    }

    /// Records the HUD into `encoder`, drawing over `view`, whose size and
    /// pixels per point `screen` gives; `stats` are the stats card's rows.
    /// Returns command buffers that must be submitted before `encoder`'s.
    pub fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        screen: &egui_wgpu::ScreenDescriptor,
        stats: &[(String, String)],
    ) -> Vec<wgpu::CommandBuffer> {
        let [width, height] = screen.size_in_pixels;
        let scale = screen.pixels_per_point;
        let now = Instant::now();
        let bar = self.bar_up(now);
        self.bar_painted = bar;
        if self.note.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.note = None;
        }
        let events = std::mem::take(&mut self.events);
        self.repaint_at = None;
        if !self.visible {
            self.bar_rect = None;
            return Vec::new();
        }
        let size = vec2(width as f32 / scale, height as f32 / scale);
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(self.started.elapsed().as_secs_f64()),
            events,
            ..Default::default()
        };
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(scale);

        let mut bar_rect = None;
        let mut actions = Vec::new();
        let mut output = self.ctx.run_ui(input, |root| {
            let ctx = root.ctx().clone();
            if self.preview {
                test_picture(
                    &ctx.layer_painter(egui::LayerId::background()),
                    Rect::from_min_size(Pos2::ZERO, size),
                );
            }
            if let Some(session) = &self.session
                && let Some(lines) = centre_message(session, self.waiting)
            {
                centre_card(&ctx, session, &lines, self.icons, size);
            }
            if bar && let Some(session) = &self.session {
                let shown = egui::Area::new(egui::Id::new("session"))
                    .anchor(Align2::CENTER_TOP, vec2(0.0, 12.0))
                    .show(&ctx, |ui| {
                        session_bar(
                            ui,
                            session,
                            BarView {
                                note: self.note.as_ref().map(|(n, _)| n.as_str()),
                                fullscreen: self.fullscreen,
                                stats: self.show_stats,
                                volume: self.volume,
                                shortcuts: &self.shortcuts,
                                mouse_hint: self.mouse_hint.as_deref(),
                                icons: self.icons,
                                width: (size.x - 32.0).clamp(BAR_MIN, BAR_MAX) - 28.0,
                                wide: size.x >= 520.0,
                            },
                            &mut actions,
                        )
                    });
                bar_rect = Some(shown.response.rect);
            }
            if self.show_stats {
                let below_bar = bar_rect.map_or(0.0, |r| r.bottom() + 8.0);
                let room = size.y - 12.0 - below_bar.max(12.0);
                let hide = self.shortcuts.binding(Shortcut::ToggleHud).keys();
                egui::Area::new(egui::Id::new("stats"))
                    .anchor(Align2::LEFT_BOTTOM, vec2(12.0, -12.0))
                    .interactable(false)
                    .show(&ctx, |ui| {
                        stats_card(ui, stats, &hide, (size.x - 24.0).min(560.0), room)
                    });
            }
        });
        self.bar_rect = bar_rect;
        self.repaint_at = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .filter(|delay| *delay < Duration::from_secs(1))
            .map(|delay| now + delay);
        for action in actions {
            match action {
                BarClick::Action(a) => self.actions.push(a),
                BarClick::Stats => {
                    self.show_stats = !self.show_stats;
                    self.repaint_at = Some(now);
                }
            }
        }

        // Drained, not just read: epaint checks (in debug builds) that every
        // texture change was applied.
        let mut textures = std::mem::take(&mut output.textures_delta);
        let jobs = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        for (id, deltas) in textures.set.drain() {
            for delta in &deltas {
                self.renderer.update_texture(device, queue, id, delta);
            }
        }
        let extra = self
            .renderer
            .update_buffers(device, queue, encoder, &jobs, screen);
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("hud"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            self.renderer.render(&mut pass, &jobs, screen);
        }
        for id in textures.free.drain() {
            self.renderer.free_texture(&id);
        }
        extra
    }
}

enum BarClick {
    Action(HudAction),
    Stats,
}

// The palette: the main window's dark theme (`apps/desktop/src/styles/
// tokens.css`), made to sit over any game.
const BAR: Color32 = Color32::from_rgba_premultiplied(29, 28, 27, 248);
const BORDER: Color32 = Color32::from_rgb(0x3a, 0x39, 0x36);
const CONTROL: Color32 = Color32::from_rgb(0x2b, 0x2a, 0x27);
const CONTROL_HOVER: Color32 = Color32::from_rgb(0x36, 0x35, 0x31);
const CONTROL_ACTIVE: Color32 = Color32::from_rgb(0x41, 0x40, 0x3b);
const CHIP_EDGE: Color32 = Color32::from_rgb(0x34, 0x33, 0x30);
const TEXT: Color32 = Color32::from_rgb(0xfa, 0xf9, 0xf5);
const MUTED: Color32 = Color32::from_rgb(0xc2, 0xc0, 0xb6);
const FAINT: Color32 = Color32::from_rgb(0x9c, 0x9a, 0x92);
/// Interaction and this viewer's own seats.
const BLUE: Color32 = Color32::from_rgb(0x4a, 0x9f, 0xf0);
const BLUE_TINT: Color32 = Color32::from_rgb(0x1f, 0x33, 0x47);
/// LIVE and Leave.
const RED: Color32 = Color32::from_rgb(0xf1, 0x54, 0x4f);
const RED_TINT: Color32 = Color32::from_rgb(0x3d, 0x22, 0x20);
/// Only "the game isn't in front".
const AMBER: Color32 = Color32::from_rgb(0xe9, 0xc2, 0x55);
/// "You're playing".
const GREEN: Color32 = Color32::from_rgb(0x57, 0xc7, 0x85);

/// The bar's width, in points, frame included.
const BAR_MIN: f32 = 300.0;
const BAR_MAX: f32 = 680.0;
/// Its rows' heights, fixed so it never jumps.
const HEADER_H: f32 = 28.0;
const CHIP_H: f32 = 40.0;
const STATUS_H: f32 = 24.0;
const HINT_H: f32 = 16.0;

const SEMIBOLD: &str = "semibold";
const ICONS: &str = "icons";

// Icons, from Segoe Fluent Icons or else Segoe MDL2 Assets: only code points
// both have.
const ICON_VOLUME: char = '\u{E767}';
const ICON_MUTED: char = '\u{E74F}';
const ICON_FULL_SCREEN: char = '\u{E740}';
const ICON_WINDOW: char = '\u{E73F}';
const ICON_STATS: char = '\u{E9D9}';
const ICON_CONTROLLER: char = '\u{E7FC}';
const ICON_KEYBOARD: char = '\u{E765}';
const ICON_WAIT: char = '\u{E916}';
const ICON_PAUSE: char = '\u{E769}';

fn semibold() -> FontFamily {
    FontFamily::Name(SEMIBOLD.into())
}

fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICONS.into()))
}

/// Windows' own fonts, read once (never bundled): Segoe UI Variable, its
/// semibold, Segoe's icons and Cascadia Mono for numbers. What is missing
/// falls back to egui's fonts; `true` with it if an icon font loaded.
fn fonts() -> (egui::FontDefinitions, bool) {
    let mut defs = egui::FontDefinitions::default();
    let dir = std::env::var_os("WINDIR")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("Fonts");
    let read = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| std::fs::read(dir.join(name)).ok())
    };
    let weight = |w: f32| egui::FontTweak {
        coords: VariationCoords::new([(b"wght", w)]),
        ..Default::default()
    };
    let (regular, bold) = match read(&["SegUIVar.ttf"]) {
        Some(variable) => (
            Some(egui::FontData::from_owned(variable.clone()).tweak(weight(400.0))),
            Some(egui::FontData::from_owned(variable).tweak(weight(600.0))),
        ),
        None => (
            read(&["segoeui.ttf"]).map(egui::FontData::from_owned),
            read(&["seguisb.ttf"]).map(egui::FontData::from_owned),
        ),
    };
    let mono = read(&["CascadiaMono.ttf", "consola.ttf"]).map(egui::FontData::from_owned);
    let icons = read(&["SegoeIcons.ttf", "segmdl2.ttf"]).map(egui::FontData::from_owned);
    let missing: Vec<&str> = [
        ("Segoe UI", regular.is_none()),
        ("its semibold", bold.is_none()),
        ("Cascadia Mono", mono.is_none()),
        ("Segoe's icons", icons.is_none()),
    ]
    .into_iter()
    .filter_map(|(name, gone)| gone.then_some(name))
    .collect();
    if !missing.is_empty() {
        tracing::info!("the HUD uses egui's own fonts for {}", missing.join(", "));
    }

    let mut first = |family: FontFamily, name: &str, data: Option<egui::FontData>| {
        if let Some(data) = data {
            defs.font_data.insert(name.into(), Arc::new(data));
            defs.families
                .entry(family)
                .or_default()
                .insert(0, name.into());
        }
    };
    first(FontFamily::Proportional, "ui", regular);
    first(FontFamily::Monospace, "mono", mono);
    let mut bold_family = defs.families[&FontFamily::Proportional].clone();
    if let Some(bold) = bold {
        defs.font_data.insert("ui-semibold".into(), Arc::new(bold));
        bold_family.insert(0, "ui-semibold".into());
    }
    defs.families.insert(semibold(), bold_family);
    // Kept off every fallback chain, its letters aren't for text; the text
    // fonts after it give it the replacement glyph egui looks for.
    let has_icons = icons.is_some();
    let mut icon_family = defs.families[&FontFamily::Proportional].clone();
    if let Some(icons) = icons {
        defs.font_data.insert("icons".into(), Arc::new(icons));
        icon_family.insert(0, "icons".into());
    }
    defs.families
        .insert(FontFamily::Name(ICONS.into()), icon_family);
    (defs, has_icons)
}

fn set_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        use egui::TextStyle;
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Heading, FontId::new(15.0, semibold())),
            (TextStyle::Monospace, FontId::monospace(12.0)),
        ]
        .into();
        let spacing = &mut style.spacing;
        spacing.item_spacing = vec2(6.0, 8.0);
        spacing.button_padding = vec2(9.0, 3.0);
        spacing.interact_size.y = 26.0;
        spacing.slider_width = 92.0;
        spacing.slider_rail_height = 4.0;
        spacing.tooltip_width = 320.0;
        style.interaction.tooltip_delay = 0.35;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = Color32::TRANSPARENT;
        v.window_fill = Color32::from_rgb(0x26, 0x26, 0x24);
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(8);
        v.popup_shadow = Shadow {
            offset: [0, 2],
            blur: 8,
            spread: 0,
            color: Color32::from_black_alpha(90),
        };
        v.selection.bg_fill = BLUE;
        v.selection.stroke = Stroke::new(1.0, TEXT);
        let w = &mut v.widgets;
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        for (state, fill, stroke) in [
            (&mut w.inactive, CONTROL, Stroke::NONE),
            (&mut w.hovered, CONTROL_HOVER, Stroke::new(1.0, BORDER)),
            (&mut w.active, CONTROL_ACTIVE, Stroke::new(1.0, BLUE)),
        ] {
            state.weak_bg_fill = fill;
            state.bg_fill = fill;
            state.bg_stroke = stroke;
            state.fg_stroke = Stroke::new(1.5, TEXT);
            state.corner_radius = CornerRadius::same(6);
            state.expansion = 0.0;
        }
    });
}

/// What the bar shows besides the session.
struct BarView<'a> {
    note: Option<&'a str>,
    fullscreen: bool,
    stats: bool,
    volume: StreamVolume,
    shortcuts: &'a Shortcuts,
    mouse_hint: Option<&'a str>,
    icons: bool,
    /// The rows' width, in points.
    width: f32,
    /// Room for the volume slider.
    wide: bool,
}

fn session_bar(ui: &mut Ui, s: &HudState, view: BarView<'_>, clicks: &mut Vec<BarClick>) {
    egui::Frame::new()
        .fill(BAR)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(14, 10))
        .shadow(Shadow {
            offset: [0, 2],
            blur: 8,
            spread: 0,
            color: Color32::from_black_alpha(70),
        })
        .show(ui, |ui| {
            ui.set_width(view.width);
            header(ui, s, &view, clicks);
            seats(ui, s, &view, clicks);
            status(ui, s, &view, clicks);
            hints(ui, &view);
        });
}

/// A row of fixed height, laid out from the right.
fn row<R>(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        vec2(width, height),
        Layout::right_to_left(Align::Center),
        |ui| {
            ui.set_min_size(vec2(width, height));
            add(ui)
        },
    )
    .inner
}

/// One line of text, cut with "…" past `width`.
fn one_line(text: &str, font: FontId, color: Color32, width: f32) -> LayoutJob {
    let mut job = LayoutJob::simple_singleline(text.into(), font, color);
    job.wrap = TextWrapping::truncate_at_width(width.max(1.0));
    job
}

/// A rounded label, cut past `max_width`.
fn pill(ui: &mut Ui, text: &str, font: FontId, colour: Color32, fill: Color32, max_width: f32) {
    let pad = vec2(8.0, 2.0);
    let galley = ui
        .painter()
        .layout_job(one_line(text, font, colour, max_width - 2.0 * pad.x));
    let (rect, response) = ui.allocate_exact_size(galley.size() + 2.0 * pad, Sense::hover());
    ui.painter().rect_filled(rect, rect.height() / 2.0, fill);
    if galley.elided {
        response.on_hover_text(text);
    }
    ui.painter().galley(rect.min + pad, galley, colour);
}

/// An icon button, or a word without the icon font.
fn icon_button(ui: &mut Ui, icons: bool, icon: char, word: &str, on: bool) -> egui::Response {
    let colour = if on { BLUE } else { TEXT };
    let button = if icons {
        egui::Button::new(
            RichText::new(icon.to_string())
                .font(icon_font(14.0))
                .color(colour),
        )
        .min_size(vec2(30.0, 26.0))
    } else {
        egui::Button::new(RichText::new(word).color(colour)).min_size(vec2(0.0, 26.0))
    };
    ui.add(if on { button.fill(BLUE_TINT) } else { button })
}

/// LIVE, the title, the host and how many watch; the volume, full screen,
/// the stats and, apart, Leave.
fn header(ui: &mut Ui, s: &HudState, view: &BarView<'_>, clicks: &mut Vec<BarClick>) {
    let keys = |shortcut| view.shortcuts.binding(shortcut).keys();
    row(ui, HEADER_H, |ui| {
        let leave = egui::Button::new(RichText::new("Leave").color(RED))
            .fill(RED_TINT)
            .min_size(vec2(0.0, 26.0));
        if ui
            .add(leave)
            .on_hover_text("Stop watching and close this window")
            .clicked()
        {
            clicks.push(BarClick::Action(HudAction::Session(UserAction::Leave)));
        }
        ui.separator();
        let stats = icon_button(ui, view.icons, ICON_STATS, "Stats", view.stats);
        if stats
            .on_hover_text(if view.stats {
                "Hide the stats"
            } else {
                "Show the stats"
            })
            .clicked()
        {
            clicks.push(BarClick::Stats);
        }
        let (icon, word, tip) = if view.fullscreen {
            (ICON_WINDOW, "Window", "Back to a window")
        } else {
            (ICON_FULL_SCREEN, "Full screen", "Full screen")
        };
        if icon_button(ui, view.icons, icon, word, false)
            .on_hover_text(format!("{tip} · {}", keys(Shortcut::Fullscreen)))
            .clicked()
        {
            clicks.push(BarClick::Action(HudAction::Fullscreen));
        }
        // The stream's volume, here only (P6): never louder than the host
        // sent it. No value box: it would take typing.
        let volume = view.volume;
        if view.wide {
            let percent_colour = if volume.muted { FAINT } else { MUTED };
            ui.add_sized(
                [34.0, 20.0],
                egui::Label::new(
                    RichText::new(format!("{}%", volume.percent))
                        .monospace()
                        .color(percent_colour),
                ),
            );
            let mut percent = volume.percent.min(StreamVolume::MAX);
            let slider = egui::Slider::new(&mut percent, 0..=StreamVolume::MAX)
                .show_value(false)
                .trailing_fill(true);
            let changed = ui
                .scope(|ui| {
                    let w = &mut ui.visuals_mut().widgets;
                    w.inactive.bg_fill = Color32::from_rgb(0x4a, 0x49, 0x45);
                    w.hovered.bg_fill = TEXT;
                    w.active.bg_fill = TEXT;
                    ui.add_enabled(!volume.muted, slider)
                        .on_hover_text(format!(
                            "Volume · {} {}",
                            keys(Shortcut::VolumeDown),
                            keys(Shortcut::VolumeUp)
                        ))
                        .changed()
                })
                .inner;
            if changed {
                clicks.push(BarClick::Action(HudAction::Volume(StreamVolume {
                    percent,
                    ..volume
                })));
            }
        }
        let (icon, word, tip) = if volume.muted {
            (ICON_MUTED, "Unmute", "Unmute")
        } else {
            (ICON_VOLUME, "Mute", "Mute")
        };
        let tip = if view.wide {
            format!("{tip} · {}", keys(Shortcut::Mute))
        } else {
            format!(
                "{tip} (volume {}%) · {}",
                volume.percent,
                keys(Shortcut::Mute)
            )
        };
        if icon_button(ui, view.icons, icon, word, false)
            .on_hover_text(tip)
            .clicked()
        {
            clicks.push(BarClick::Action(HudAction::Volume(StreamVolume {
                muted: !volume.muted,
                ..volume
            })));
        }

        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            let small = FontId::new(11.0, semibold());
            match s.stream {
                StreamState::Live => pill(ui, "LIVE", small, Color32::WHITE, RED, 80.0),
                _ => pill(ui, "NOT STREAMING", small, MUTED, CONTROL_ACTIVE, 120.0),
            }
            let watching = match s.viewers {
                1 => "1 watching".to_string(),
                n => format!("{n} watching"),
            };
            let mut job = LayoutJob::default();
            job.append(
                &s.title,
                0.0,
                TextFormat::simple(FontId::new(14.5, semibold()), TEXT),
            );
            job.append(
                &format!(" · {} · {watching}", s.host),
                0.0,
                TextFormat::simple(FontId::proportional(13.5), MUTED),
            );
            ui.add(egui::Label::new(job).truncate());
        });
    });
}

/// A seat as its chip shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Chip {
    /// `None` for the host playing beside the seats.
    seat: Option<u8>,
    label: String,
    /// Who has it; `None` while free.
    who: Option<String>,
    host: bool,
    pad: bool,
    mine: bool,
    asked: bool,
}

/// A chip per seat. One seat nobody holds, while the host plays beside the
/// seats, reads the host: they play it on whatever the game sees directly.
/// With several, such a host gets a chip of their own after the seats.
fn seat_chips(s: &HudState) -> Vec<Chip> {
    let mut chips: Vec<Chip> = s
        .seats
        .iter()
        .enumerate()
        .map(|(i, seat)| Chip {
            seat: Some(i as u8),
            label: seat.label.clone(),
            who: seat.holder.clone(),
            host: seat.host,
            pad: seat.pad,
            mine: seat.mine,
            asked: seat.asked,
        })
        .collect();
    if !s.host_on_seats {
        if let [only] = chips.as_mut_slice() {
            if only.who.is_none() {
                only.who = Some(s.host.clone());
                only.host = true;
            }
        } else {
            chips.push(Chip {
                seat: None,
                label: "Own controller".into(),
                who: Some(s.host.clone()),
                host: true,
                pad: true,
                mine: false,
                asked: false,
            });
        }
    }
    chips
}

fn seats(ui: &mut Ui, s: &HudState, view: &BarView<'_>, clicks: &mut Vec<BarClick>) {
    let chips = seat_chips(s);
    if chips.is_empty() {
        return;
    }
    let several_mine = s.seats.iter().filter(|seat| seat.mine).count() > 1;
    let gap = 6.0;
    let n = chips.len() as f32;
    let width = ((view.width - gap * (n - 1.0)) / n).min(240.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for chip in &chips {
            seat_chip(ui, chip, s, width, several_mine, view.icons, clicks);
        }
    });
}

/// A chip: the seat on top, who has it below. A seat this viewer could ask
/// for is a button; its own seats aren't (with several, each has a Give
/// back of its own).
fn seat_chip(
    ui: &mut Ui,
    c: &Chip,
    s: &HudState,
    width: f32,
    several_mine: bool,
    icons: bool,
    clicks: &mut Vec<BarClick>,
) {
    let askable = c.seat.is_some() && !c.mine && !c.asked;
    let enabled = askable && !s.control_paused;
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(width, CHIP_H), sense);
    let hovered = enabled && response.hovered();
    let (fill, stroke) = if c.mine {
        (BLUE_TINT, Stroke::new(1.0, BLUE.gamma_multiply(0.6)))
    } else if c.asked {
        (CONTROL, Stroke::new(1.5, BLUE))
    } else if hovered {
        (CONTROL_HOVER, Stroke::new(1.0, BLUE))
    } else {
        (CONTROL, Stroke::new(1.0, CHIP_EDGE))
    };
    let painter = ui.painter_at(rect);
    painter.rect(rect, 6.0, fill, stroke, StrokeKind::Inside);

    let inner = rect.shrink2(vec2(9.0, 5.0));
    let give_back = c.mine && several_mine;
    let button_w = if give_back { 64.0 } else { 0.0 };
    let mut x = inner.left();
    // A narrow chip keeps its words.
    if icons && width >= 110.0 {
        let icon = if c.pad {
            ICON_CONTROLLER
        } else {
            ICON_KEYBOARD
        };
        painter.text(
            pos2(x, inner.top() + 7.0),
            Align2::LEFT_CENTER,
            icon,
            icon_font(12.0),
            FAINT,
        );
        x += 17.0;
    }
    let label = painter.layout_job(one_line(
        &c.label,
        FontId::proportional(11.5),
        MUTED,
        inner.right() - x,
    ));
    painter.galley(pos2(x, inner.top()), label, MUTED);

    let width = inner.width() - button_w - if give_back { 4.0 } else { 0.0 };
    let body = FontId::proportional(13.5);
    let bold = FontId::new(13.5, semibold());
    let mut job = match (&c.who, c.mine) {
        (_, true) => one_line("You", bold, BLUE, width),
        (None, _) if c.asked => one_line("Asked…", bold, BLUE, width),
        (None, _) => one_line("Free", body, FAINT, width),
        (Some(who), _) => one_line(who, body, TEXT, width),
    };
    let small = FontId::proportional(11.5);
    if c.asked && c.who.is_some() {
        job.append(" · asked", 0.0, TextFormat::simple(small, BLUE));
    } else if c.host && c.seat.is_some() && !c.mine {
        job.append(" · host", 0.0, TextFormat::simple(small, FAINT));
    }
    let who = painter.layout_job(job);
    painter.galley(pos2(inner.left(), inner.bottom() - who.size().y), who, TEXT);

    if give_back && let Some(seat) = c.seat {
        let at = Rect::from_min_size(
            pos2(inner.right() - button_w, inner.bottom() - 20.0),
            vec2(button_w, 20.0),
        );
        let button = egui::Button::new(RichText::new("Give back").small())
            .wrap_mode(egui::TextWrapMode::Extend)
            .min_size(vec2(button_w, 20.0));
        // Inside the chip: egui's taller buttons would push the next chips.
        let tall = std::mem::replace(&mut ui.spacing_mut().interact_size.y, 20.0);
        let response = ui.put(at, button);
        ui.spacing_mut().interact_size.y = tall;
        if response
            .on_hover_text(format!("Give back {}", seat_phrase(&c.label)))
            .clicked()
        {
            clicks.push(BarClick::Action(HudAction::Session(UserAction::GiveBack(
                seat,
            ))));
        }
    }

    if enabled
        && response.clicked()
        && let Some(seat) = c.seat
    {
        clicks.push(BarClick::Action(HudAction::Session(
            UserAction::RequestControl(seat),
        )));
    }
    let phrase = seat_phrase(&c.label);
    let tip = match c.seat {
        None => Some(format!(
            "{} plays beside the seats, on a controller the game sees directly",
            s.host
        )),
        Some(_) if c.mine => None,
        Some(_) if c.asked => Some(format!("Waiting for {} to answer", s.host)),
        Some(_) if s.control_paused => Some(format!("{} paused control for now", s.host)),
        Some(_) => Some(match &c.who {
            None => format!("Ask {} for {phrase}", s.host),
            Some(who) if c.host => format!("Ask {who} for {phrase}: they play it now"),
            Some(who) => format!("Ask {} for {phrase}: {who} has it now", s.host),
        }),
    };
    if let Some(tip) = tip {
        response.on_hover_text(tip);
    }
}

/// Where this viewer stands, with the note beside it and the request's or
/// the seats' button at the end; then, only when it applies, that the game
/// isn't in front.
fn status(ui: &mut Ui, s: &HudState, view: &BarView<'_>, clicks: &mut Vec<BarClick>) {
    let mine: Vec<&HudSeat> = s.seats.iter().filter(|seat| seat.mine).collect();
    let asked = s.seats.iter().find(|seat| seat.asked);
    let (line, dot) = match (s.role, mine.as_slice()) {
        (HudRole::Asking, []) => (format!("Waiting for {} to answer…", s.host), BLUE),
        // Playing, and maybe asking for another seat meanwhile.
        (_, [_, ..]) => {
            let mut line = playing_with(s, &mine);
            if let Some(asked) = asked {
                line.push_str(&format!(" · asked for {}", seat_phrase(&asked.label)));
            }
            (line, GREEN)
        }
        _ if s.control_paused => (format!("{} paused control for now", s.host), FAINT),
        _ => ("You're watching".to_string(), FAINT),
    };
    let cancel = match s.role {
        HudRole::Asking => Some("Withdraw the request"),
        HudRole::Controlling if mine.len() > 1 => Some("Give all back"),
        HudRole::Controlling => Some("Give it back"),
        HudRole::Watching => None,
    };
    // A note goes beside the line if it fits there, else on a row of its
    // own: it says what just happened.
    let note_width = |note: &str| {
        let job = one_line(note, note_font(), TEXT, f32::INFINITY);
        ui.painter().layout_job(job).size().x + 16.0
    };
    let (note_beside, note_below) = match view.note {
        Some(note) if note_width(note) <= view.width * 0.4 => (Some(note), None),
        note => (None, note),
    };
    row(ui, STATUS_H, |ui| {
        if let Some(cancel) = cancel
            && ui.button(cancel).clicked()
        {
            clicks.push(BarClick::Action(HudAction::Session(
                UserAction::CancelControl,
            )));
        }
        if let Some(note) = note_beside {
            let max = ui.available_width() * 0.45;
            pill(ui, note, note_font(), TEXT, CONTROL_ACTIVE, max);
        }
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            dot_label(ui, dot, RichText::new(line).color(TEXT));
        });
    });
    if let Some(note) = note_below {
        row(ui, STATUS_H, |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let max = ui.available_width();
                pill(ui, note, note_font(), TEXT, CONTROL_ACTIVE, max);
            });
        });
    }
    let playing_or_asking = !mine.is_empty() || s.role == HudRole::Asking;
    if !s.game_in_front && playing_or_asking {
        row(ui, HINT_H + 2.0, |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let line = format!(
                    "The game isn't in front on {}'s PC: input may not reach it",
                    s.host
                );
                dot_label(ui, AMBER, RichText::new(line).color(AMBER));
            });
        });
    }
}

fn note_font() -> FontId {
    FontId::proportional(12.5)
}

fn dot_label(ui: &mut Ui, dot: Color32, text: RichText) {
    let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, dot);
    ui.add(egui::Label::new(text).truncate());
}

/// The mouse's hint while this viewer has it, then the shortcuts.
fn hints(ui: &mut Ui, view: &BarView<'_>) {
    let shortcuts = view.shortcuts;
    let keys = format!(
        "{}: ask or give back · {}: full screen · {}: hide · {} {} {}: volume down, up, mute",
        shortcuts.binding(Shortcut::ToggleControl).keys(),
        shortcuts.binding(Shortcut::Fullscreen).key_name(),
        shortcuts.binding(Shortcut::ToggleHud).key_name(),
        shortcuts.binding(Shortcut::VolumeDown).key_name(),
        shortcuts.binding(Shortcut::VolumeUp).key_name(),
        shortcuts.binding(Shortcut::Mute).key_name(),
    );
    let small = FontId::proportional(11.5);
    let mut job = LayoutJob::default();
    if let Some(mouse) = view.mouse_hint {
        job.append(mouse, 0.0, TextFormat::simple(small.clone(), MUTED));
        job.append("  ·  ", 0.0, TextFormat::simple(small.clone(), FAINT));
    }
    job.append(&keys, 0.0, TextFormat::simple(small, FAINT));
    row(ui, HINT_H, |ui| {
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add(egui::Label::new(job).truncate());
        });
    });
}

/// What the middle of the window says, if anything: why nothing is shown
/// (M19: nothing streamed, or the last stream ended), or that the picture
/// hasn't come yet. A second, smaller line with it.
fn centre_message(s: &HudState, waiting: bool) -> Option<(String, Option<&'static str>)> {
    let wait = "Your seat stays yours; the next stream shows here.";
    let first = match s.stream {
        StreamState::Live if waiting => format!("Waiting for {}'s picture…", s.host),
        StreamState::Live => return None,
        StreamState::Waiting => format!("{} isn't streaming anything yet", s.host),
        StreamState::Stopped(EndReason::GameExited) => "The game closed".into(),
        StreamState::Stopped(EndReason::SourceLost) => {
            "The window or screen being streamed went away".into()
        }
        StreamState::Stopped(EndReason::Error) => "The stream failed".into(),
        StreamState::Stopped(_) => format!("{} stopped streaming", s.host),
    };
    let more = (s.stream != StreamState::Live).then_some(wait);
    Some((first, more))
}

fn centre_card(
    ctx: &egui::Context,
    s: &HudState,
    (first, more): &(String, Option<&'static str>),
    icons: bool,
    size: egui::Vec2,
) {
    let icon = match s.stream {
        StreamState::Stopped(_) => ICON_PAUSE,
        _ => ICON_WAIT,
    };
    egui::Area::new(egui::Id::new("centre"))
        .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(BAR)
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(12)
                .inner_margin(Margin::symmetric(22, 16))
                .show(ui, |ui| {
                    ui.set_width((size.x - 76.0).clamp(160.0, 340.0));
                    ui.vertical_centered(|ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        if icons {
                            ui.label(
                                RichText::new(icon.to_string())
                                    .font(icon_font(24.0))
                                    .color(FAINT),
                            );
                        }
                        ui.label(
                            RichText::new(first)
                                .font(FontId::new(15.5, semibold()))
                                .color(TEXT),
                        );
                        if let Some(more) = more {
                            ui.label(RichText::new(*more).color(MUTED));
                        }
                    });
                });
        });
}

/// The stats card: a row per measure, as many as `room` (points) holds.
fn stats_card(ui: &mut Ui, rows: &[(String, String)], hide: &str, width: f32, room: f32) {
    const LABEL_W: f32 = 112.0;
    egui::Frame::new()
        .fill(BAR)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(width - 24.0);
            let top = ui.cursor().top();
            row(ui, 18.0, |ui| {
                ui.label(
                    RichText::new(format!("{hide} hides all this"))
                        .small()
                        .color(FAINT),
                );
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.label(RichText::new("Stream").font(FontId::new(13.5, semibold())));
                });
            });
            ui.spacing_mut().item_spacing.y = 3.0;
            for (i, (label, value)) in rows.iter().enumerate() {
                if ui.cursor().top() - top > room - 60.0 {
                    let more = rows.len() - i;
                    ui.label(
                        RichText::new(format!("{more} more in a taller window"))
                            .small()
                            .color(FAINT),
                    );
                    break;
                }
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        vec2(LABEL_W, 14.0),
                        Layout::left_to_right(Align::Min),
                        |ui| {
                            ui.set_min_width(LABEL_W);
                            ui.label(RichText::new(label).small().color(FAINT));
                        },
                    );
                    ui.add(egui::Label::new(RichText::new(value).monospace().color(TEXT)).wrap());
                });
            }
        });
}

/// What a preview draws behind the HUD, to judge the bar over a picture: a
/// sky, a white band (the worst case behind text) and some shapes.
fn test_picture(painter: &egui::Painter, rect: Rect) {
    let mut mesh = egui::Mesh::default();
    let top = Color32::from_rgb(0x2f, 0x5d, 0x96);
    let bottom = Color32::from_rgb(0xd8, 0x8f, 0x4e);
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
    let band = Rect::from_min_max(
        pos2(rect.left(), rect.top() + 56.0),
        pos2(rect.center().x, rect.top() + 110.0),
    );
    painter.rect_filled(band, 0.0, Color32::from_rgb(0xf4, 0xf4, 0xf0));
    painter.circle_filled(
        rect.center() + vec2(-rect.width() * 0.25, rect.height() * 0.15),
        rect.height() * 0.12,
        Color32::from_rgb(0xf2, 0xd3, 0x4a),
    );
    let ground = Rect::from_min_max(
        pos2(rect.left(), rect.bottom() - rect.height() * 0.22),
        rect.right_bottom(),
    );
    painter.rect_filled(ground, 0.0, Color32::from_rgb(0x2e, 0x6b, 0x3a));
    let cell = 24.0;
    for i in 0..8 {
        for j in 0..4 {
            if (i + j) % 2 == 0 {
                let min = pos2(
                    rect.right() - (i + 1) as f32 * cell - 24.0,
                    rect.top() + 140.0 + j as f32 * cell,
                );
                painter.rect_filled(
                    Rect::from_min_size(min, vec2(cell, cell)),
                    0.0,
                    Color32::from_gray(20),
                );
            }
        }
    }
}

/// What this viewer plays, on `mine` (at least one seat): with several (M18),
/// "You're playing Controller 1 and Controller 2", the input line saying
/// which controller drives which.
fn playing_with(s: &HudState, mine: &[&HudSeat]) -> String {
    match mine {
        [seat] => {
            let with = match (seat.pad, seat.keyboard_mouse) {
                (true, true) => "your gamepad, keyboard and mouse",
                (false, true) => "your keyboard and mouse",
                _ => "your gamepad",
            };
            if s.seats.len() > 1 {
                format!("You're playing as {}, with {with}", seat.label)
            } else {
                format!("You're playing, with {with}")
            }
        }
        seats => {
            let labels: Vec<&str> = seats.iter().map(|seat| seat.label.as_str()).collect();
            let (last, rest) = labels.split_last().expect("several seats");
            format!("You're playing {} and {last}", rest.join(", "))
        }
    }
}

/// The seat Ctrl+Alt+Shift+C asks for: the first nobody holds, the
/// controllers coming first; with none free, the first that isn't ours.
pub fn seat_to_ask(s: &HudState) -> Option<u8> {
    let free = s
        .seats
        .iter()
        .position(|seat| seat.holder.is_none() && !seat.mine);
    let other = s.seats.iter().position(|seat| !seat.mine);
    free.or(other).map(|i| i as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(label: &str, holder: Option<&str>, host: bool) -> HudSeat {
        HudSeat {
            label: label.into(),
            pad: true,
            keyboard_mouse: false,
            holder: holder.map(Into::into),
            host,
            mine: false,
            asked: false,
        }
    }

    fn session(seats: Vec<HudSeat>, host_on_seats: bool) -> HudState {
        HudState {
            title: "Fling to the Finish".into(),
            host: "Ana".into(),
            role: HudRole::Watching,
            seats,
            host_on_seats,
            viewers: 1,
            control_paused: false,
            game_in_front: true,
            stream: StreamState::Live,
            note: String::new(),
        }
    }

    /// Who each chip names, `None` for a free seat; the host's chip beside
    /// the seats has no seat.
    fn who(s: &HudState) -> Vec<(Option<u8>, Option<String>, bool)> {
        seat_chips(s)
            .into_iter()
            .map(|c| (c.seat, c.who, c.host))
            .collect()
    }

    /// M19: the middle of the window says why nothing is shown.
    #[test]
    fn the_window_says_why_nothing_is_shown() {
        let mut s = session(vec![seat("Controller 1", None, false)], false);
        assert_eq!(centre_message(&s, false), None, "a picture is shown");
        assert_eq!(
            centre_message(&s, true).map(|m| m.0),
            Some("Waiting for Ana's picture…".into())
        );
        s.stream = StreamState::Waiting;
        let (first, more) = centre_message(&s, true).unwrap();
        assert_eq!(first, "Ana isn't streaming anything yet");
        assert!(more.is_some(), "and the seat stays");
        s.stream = StreamState::Stopped(EndReason::GameExited);
        assert_eq!(centre_message(&s, true).unwrap().0, "The game closed");
        s.stream = StreamState::Stopped(EndReason::HostStopped);
        assert_eq!(centre_message(&s, true).unwrap().0, "Ana stopped streaming");
    }

    #[test]
    fn the_host_shows_on_their_seat_or_beside_the_seats() {
        let ana = || Some("Ana".to_string());
        let bia = || Some("Bia".to_string());
        let on_a_seat = session(
            vec![
                seat("Controller 1", Some("Ana"), true),
                seat("Controller 2", None, false),
            ],
            true,
        );
        assert_eq!(
            who(&on_a_seat),
            [(Some(0), ana(), true), (Some(1), None, false)]
        );
        let beside = session(
            vec![
                seat("Controller 1", Some("Bia"), false),
                seat("Controller 2", None, false),
            ],
            false,
        );
        assert_eq!(
            who(&beside),
            [
                (Some(0), bia(), false),
                (Some(1), None, false),
                (None, ana(), true)
            ]
        );
        // On seats but playing none (the host passed theirs): no chip of
        // their own, since their controller is hidden from the game.
        let on_none = session(
            vec![
                seat("Controller 1", Some("Bia"), false),
                seat("Controller 2", None, false),
            ],
            true,
        );
        assert_eq!(
            who(&on_none),
            [(Some(0), bia(), false), (Some(1), None, false)]
        );
        // A single seat nobody holds, the host playing beside it: it reads
        // the host, and can still be asked for.
        let single = session(vec![seat("Controller", None, false)], false);
        assert_eq!(who(&single), [(Some(0), ana(), true)]);
        // Held by a guest: the guest, and no chip for the host.
        let taken = session(vec![seat("Controller", Some("Bia"), false)], false);
        assert_eq!(who(&taken), [(Some(0), bia(), false)]);
        let empty = session(vec![seat("Controller", None, false)], true);
        assert_eq!(who(&empty), [(Some(0), None, false)]);
        // The free seat is the one to ask for, not the host's.
        assert_eq!(seat_to_ask(&on_a_seat), Some(1));
    }

    /// M18: a viewer on several seats hears all of them.
    #[test]
    fn several_seats_are_named_together() {
        let mut one = seat("Controller 1", Some("Bia"), false);
        one.mine = true;
        let mut two = seat("Controller 2", Some("Bia"), false);
        two.mine = true;
        let mut keys = seat("Keyboard and mouse", Some("Bia"), false);
        keys.mine = true;
        keys.pad = false;
        keys.keyboard_mouse = true;
        let s = session(vec![one.clone(), two.clone(), keys.clone()], false);
        assert_eq!(
            playing_with(&s, &[&one]),
            "You're playing as Controller 1, with your gamepad"
        );
        assert_eq!(
            playing_with(&s, &[&one, &two]),
            "You're playing Controller 1 and Controller 2"
        );
        assert_eq!(
            playing_with(&s, &[&one, &two, &keys]),
            "You're playing Controller 1, Controller 2 and Keyboard and mouse"
        );
    }

    /// The bar's text needs fonts that load: whatever this PC has, every
    /// family the bar names exists (egui panics on an unknown one).
    #[test]
    fn every_family_the_bar_names_exists() {
        let (defs, _) = fonts();
        for family in [
            FontFamily::Proportional,
            FontFamily::Monospace,
            semibold(),
            FontFamily::Name(ICONS.into()),
        ] {
            let fonts = &defs.families[&family];
            assert!(!fonts.is_empty(), "{family:?}");
            for name in fonts {
                assert!(defs.font_data.contains_key(name), "{name}");
            }
        }
    }
}
