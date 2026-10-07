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
use couch_proto::{
    ipc::{HudRole, HudSeat, HudState, UserAction},
    session::seat_phrase,
};
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
            (o.role, &o.seats, o.control_paused, o.game_in_front)
                != (
                    state.role,
                    &state.seats,
                    state.control_paused,
                    state.game_in_front,
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
        // Up while asking (unless playing meanwhile, asking to move: the bar
        // would cover the game), and while playing with the game out of
        // front on the host: the player must know why input does nothing.
        let playing = session.seats.iter().any(|seat| seat.mine);
        let asking = session.role == HudRole::Asking && !playing;
        let away = playing && !session.game_in_front;
        self.visible
            && (asking
                || away
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
            let watching = match s.viewers {
                1 => "1 watching".to_string(),
                n => format!("{n} watching"),
            };
            ui.label(format!("{} · {watching}", who_plays(s)));

            let mine = s.seats.iter().find(|seat| seat.mine);
            let asked = s.seats.iter().find(|seat| seat.asked);
            let (state, colour) = match (s.role, mine) {
                // Playing, and maybe asking to move meanwhile.
                (_, Some(seat)) => {
                    let mut state = playing_with(s, seat);
                    if let Some(asked) = asked {
                        state.push_str(&format!(" · asked for {}", seat_phrase(&asked.label)));
                    }
                    (state, Color32::LIGHT_GREEN)
                }
                (HudRole::Asking, None) => (
                    format!("Waiting for {} to answer…", s.host),
                    Color32::LIGHT_BLUE,
                ),
                _ if s.control_paused => {
                    (format!("{} paused control for now", s.host), Color32::GRAY)
                }
                _ => ("You're watching".to_string(), Color32::GRAY),
            };
            ui.label(RichText::new(state).color(colour));
            if !s.game_in_front {
                ui.label(
                    RichText::new(format!(
                        "The game isn't in front on {}'s PC: input may not reach it",
                        s.host
                    ))
                    .color(NOTE_YELLOW),
                );
            }
            if let Some(note) = note {
                ui.label(RichText::new(note).color(NOTE_YELLOW));
            }

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                // Ask for any seat but our own: a free one, someone else's
                // (the host decides), or another than ours (a move).
                for (i, seat) in s.seats.iter().enumerate() {
                    if seat.mine {
                        continue;
                    }
                    let text = if seat.asked {
                        format!("Asked for {}", seat_phrase(&seat.label))
                    } else {
                        format!("Ask for {}", seat_phrase(&seat.label))
                    };
                    let enabled = !seat.asked && !s.control_paused;
                    if ui.add_enabled(enabled, egui::Button::new(text)).clicked() {
                        clicks.push(BarClick::Action(HudAction::Session(
                            UserAction::RequestControl(i as u8),
                        )));
                    }
                }
                let cancel = match s.role {
                    HudRole::Asking => Some("Withdraw the request"),
                    HudRole::Controlling => Some("Give it back"),
                    HudRole::Watching => None,
                };
                if let Some(cancel) = cancel
                    && ui.button(cancel).clicked()
                {
                    clicks.push(BarClick::Action(HudAction::Session(
                        UserAction::CancelControl,
                    )));
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

/// "Bia is playing" (the host, while nobody holds the one seat and the host
/// plays outside it), or with several seats "Controller 1: Ana ·
/// Controller 2: free". A host who plays outside the seats, on whatever the
/// game sees directly, is listed beside them: "Ana: own controller".
fn who_plays(s: &HudState) -> String {
    match s.seats.as_slice() {
        [seat] => match (&seat.holder, s.host_on_seats) {
            (Some(holder), _) => format!("{holder} is playing"),
            (None, true) => "Nobody is playing".into(),
            (None, false) => format!("{} is playing", s.host),
        },
        seats => {
            let outside = (!s.host_on_seats).then(|| format!("{}: own controller", s.host));
            seats
                .iter()
                .map(|seat| {
                    format!(
                        "{}: {}",
                        seat.label,
                        seat.holder.as_deref().unwrap_or("free")
                    )
                })
                .chain(outside)
                .collect::<Vec<_>>()
                .join(" · ")
        }
    }
}

/// What this viewer plays with, on `seat`.
fn playing_with(s: &HudState, seat: &HudSeat) -> String {
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
            note: String::new(),
        }
    }

    #[test]
    fn the_host_shows_on_their_seat_or_beside_the_seats() {
        let on_a_seat = session(
            vec![
                seat("Controller 1", Some("Ana"), true),
                seat("Controller 2", None, false),
            ],
            true,
        );
        assert_eq!(
            who_plays(&on_a_seat),
            "Controller 1: Ana · Controller 2: free"
        );
        let beside = session(
            vec![
                seat("Controller 1", Some("Bia"), false),
                seat("Controller 2", None, false),
            ],
            false,
        );
        assert_eq!(
            who_plays(&beside),
            "Controller 1: Bia · Controller 2: free · Ana: own controller"
        );
        // On seats but playing none (the host passed theirs): not "own
        // controller", which is hidden from the game.
        let on_none = session(
            vec![
                seat("Controller 1", Some("Bia"), false),
                seat("Controller 2", None, false),
            ],
            true,
        );
        assert_eq!(
            who_plays(&on_none),
            "Controller 1: Bia · Controller 2: free"
        );
        let single = session(vec![seat("Controller", None, false)], false);
        assert_eq!(who_plays(&single), "Ana is playing");
        let empty = session(vec![seat("Controller", None, false)], true);
        assert_eq!(who_plays(&empty), "Nobody is playing");
        // The free seat is the one to ask for, not the host's.
        assert_eq!(seat_to_ask(&on_a_seat), Some(1));
    }
}
