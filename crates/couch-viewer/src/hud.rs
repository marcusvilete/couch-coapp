//! The in-stream HUD, drawn with egui on top of the video:
//! - the session bar: what is streaming, who plays, where this viewer
//!   stands, a note such as "the host said no", and buttons to ask for the
//!   controller, go full screen, show the stats or leave;
//! - the stats panel (latency, path, decoder).
//!
//! The bar takes clicks only while the mouse is free (not captured for the
//! game), and a click on it never reaches the game: the window loop hands
//! such clicks here instead of to the input capture.
//!
//! The bar shows for a while after anything changes and while the pointer
//! moves over the window, then gets out of the way.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use couch_input::shortcuts::{self, Shortcut};
use couch_proto::ipc::{HudRole, HudState, UserAction};
use egui::{Align2, Color32, Pos2, Rect, RichText};

/// How long the bar stays after a change, and a note with it.
const SHOW_AFTER_CHANGE: Duration = Duration::from_secs(5);
/// How long the bar stays after the pointer last moved.
const SHOW_AFTER_POINTER: Duration = Duration::from_secs(3);

/// Something the user asked for on the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudAction {
    Session(UserAction),
    Fullscreen,
}

pub struct Hud {
    ctx: egui::Context,
    renderer: egui_wgpu::Renderer,
    started: Instant,
    /// The whole HUD (Ctrl+Alt+Shift+H).
    pub visible: bool,
    pub show_stats: bool,
    /// Full screen now, for the button's label.
    pub fullscreen: bool,
    /// No picture has arrived yet.
    pub waiting: bool,
    session: Option<HudState>,
    note: Option<(String, Instant)>,
    changed_at: Option<Instant>,
    pointer_at: Option<Instant>,
    events: Vec<egui::Event>,
    /// Where the bar was last drawn, in points.
    bar_rect: Option<Rect>,
    /// Buttons pressed on the bar: their releases come here too.
    pressed: BTreeSet<u8>,
    actions: Vec<HudAction>,
    /// Whether the bar was up at the last paint, so a timeout repaints.
    bar_painted: bool,
    /// When egui wants to paint again: it lays a new area out unseen on its
    /// first frame, and animates hovers. Video repaints anyway; a still
    /// screen (no frame yet, a paused game) needs this.
    repaint_at: Option<Instant>,
}

impl Hud {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        Self {
            ctx,
            renderer: egui_wgpu::Renderer::new(
                device,
                format,
                egui_wgpu::RendererOptions::default(),
            ),
            started: Instant::now(),
            visible: true,
            show_stats: true,
            fullscreen: false,
            waiting: false,
            session: None,
            note: None,
            changed_at: None,
            pointer_at: None,
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
            (o.role, &o.controller, o.control_paused)
                != (state.role, &state.controller, state.control_paused)
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
        self.pointer_at = Some(Instant::now());
        self.events.push(egui::Event::PointerMoved(pos));
    }

    pub fn pointer_gone(&mut self) {
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
        self.visible
            && (session.role == HudRole::Asking
                || recent(self.changed_at, SHOW_AFTER_CHANGE)
                || recent(self.pointer_at, SHOW_AFTER_POINTER)
                || self.note.as_ref().is_some_and(|(_, until)| now < *until)
                || !self.pressed.is_empty())
    }

    /// Records the HUD into `encoder`, drawing over `view`, whose size and
    /// pixels per point `screen` gives. Returns command buffers that must be
    /// submitted before `encoder`'s.
    pub fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        screen: &egui_wgpu::ScreenDescriptor,
        lines: &[String],
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
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(width as f32 / scale, height as f32 / scale),
            )),
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
            if self.waiting
                && let Some(session) = &self.session
            {
                egui::Area::new(egui::Id::new("waiting"))
                    .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .interactable(false)
                    .show(&ctx, |ui| {
                        ui.label(
                            RichText::new(format!("Waiting for {}'s picture…", session.host))
                                .size(18.0)
                                .color(Color32::GRAY),
                        );
                    });
            }
            if self.show_stats {
                egui::Area::new(egui::Id::new("stats"))
                    .anchor(Align2::LEFT_BOTTOM, egui::vec2(12.0, -12.0))
                    .interactable(false)
                    .show(&ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            for line in lines {
                                ui.monospace(line);
                            }
                        });
                    });
            }
            if bar && let Some(session) = &self.session {
                let shown = egui::Area::new(egui::Id::new("session"))
                    .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 12.0))
                    .show(&ctx, |ui| {
                        session_bar(
                            ui,
                            session,
                            self.note.as_ref().map(|(n, _)| n.as_str()),
                            self.fullscreen,
                            self.show_stats,
                            &mut actions,
                        )
                    });
                bar_rect = Some(shown.response.rect);
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

/// Background of the bar: dark enough to read over any game.
const BAR_FILL: Color32 = Color32::from_rgba_premultiplied(16, 16, 20, 230);
const LIVE_RED: Color32 = Color32::from_rgb(237, 66, 69);
const NOTE_YELLOW: Color32 = Color32::from_rgb(250, 210, 90);

fn session_bar(
    ui: &mut egui::Ui,
    s: &HudState,
    note: Option<&str>,
    fullscreen: bool,
    stats: bool,
    clicks: &mut Vec<BarClick>,
) {
    egui::Frame::new()
        .fill(BAR_FILL)
        .corner_radius(8.0)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_max_width(520.0);
            ui.horizontal(|ui| {
                egui::Frame::new()
                    .fill(LIVE_RED)
                    .corner_radius(4.0)
                    .inner_margin(egui::Margin::symmetric(5, 1))
                    .show(ui, |ui| {
                        ui.label(RichText::new("LIVE").strong().color(Color32::WHITE));
                    });
                ui.label(RichText::new(&s.title).strong().size(16.0));
                ui.label(format!("· {}", s.host));
            });
            let player = s.controller.as_deref().unwrap_or(&s.host);
            let watching = match s.viewers {
                1 => "1 watching".to_string(),
                n => format!("{n} watching"),
            };
            ui.label(format!("{player} is playing · {watching}"));

            let (state, colour) = match s.role {
                HudRole::Controlling if s.keyboard_mouse => (
                    "You're playing, with your gamepad, keyboard and mouse".to_string(),
                    Color32::LIGHT_GREEN,
                ),
                HudRole::Controlling => (
                    "You're playing, with your gamepad".to_string(),
                    Color32::LIGHT_GREEN,
                ),
                HudRole::Asking => (
                    format!("Waiting for {} to answer…", s.host),
                    Color32::LIGHT_BLUE,
                ),
                HudRole::Watching if s.control_paused => {
                    (format!("{} paused control for now", s.host), Color32::GRAY)
                }
                HudRole::Watching => ("You're watching".to_string(), Color32::GRAY),
            };
            ui.label(RichText::new(state).color(colour));
            if let Some(note) = note {
                ui.label(RichText::new(note).color(NOTE_YELLOW));
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let control = match s.role {
                    HudRole::Watching => ("Ask for the controller", UserAction::RequestControl),
                    HudRole::Asking => ("Withdraw the request", UserAction::CancelControl),
                    HudRole::Controlling => ("Give it back", UserAction::CancelControl),
                };
                let enabled = !(s.role == HudRole::Watching && s.control_paused);
                if ui
                    .add_enabled(enabled, egui::Button::new(control.0))
                    .clicked()
                {
                    clicks.push(BarClick::Action(HudAction::Session(control.1)));
                }
                let screen = if fullscreen { "Window" } else { "Full screen" };
                if ui.button(screen).clicked() {
                    clicks.push(BarClick::Action(HudAction::Fullscreen));
                }
                if ui.selectable_label(stats, "Stats").clicked() {
                    clicks.push(BarClick::Stats);
                }
                if ui.button("Leave").clicked() {
                    clicks.push(BarClick::Action(HudAction::Session(UserAction::Leave)));
                }
            });
            ui.label(
                RichText::new(format!(
                    "{}: ask or give back · {}: full screen · {}: hide",
                    shortcuts::binding(Shortcut::ToggleControl).keys(),
                    shortcuts::binding(Shortcut::Fullscreen).key_name,
                    shortcuts::binding(Shortcut::ToggleHud).key_name,
                ))
                .small()
                .color(Color32::GRAY),
            );
        });
}
