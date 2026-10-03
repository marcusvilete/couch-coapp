//! The in-stream HUD, drawn with egui on top of the video. Display-only in
//! M1 (stats); interactive controls arrive with sessions in M5.

use std::time::Instant;

pub struct Hud {
    ctx: egui::Context,
    renderer: egui_wgpu::Renderer,
    started: Instant,
    pub visible: bool,
}

impl Hud {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self {
            ctx: egui::Context::default(),
            renderer: egui_wgpu::Renderer::new(
                device,
                format,
                egui_wgpu::RendererOptions::default(),
            ),
            started: Instant::now(),
            visible: true,
        }
    }

    /// Records the HUD into `encoder`, drawing over `view`. Returns command
    /// buffers that must be submitted before `encoder`'s.
    pub fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        (width, height): (u32, u32),
        lines: &[String],
    ) -> Vec<wgpu::CommandBuffer> {
        if !self.visible {
            return Vec::new();
        }
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width as f32, height as f32),
            )),
            time: Some(self.started.elapsed().as_secs_f64()),
            ..Default::default()
        };
        let output = self.ctx.run_ui(input, |root| {
            egui::Area::new(egui::Id::new("stats"))
                .fixed_pos(egui::pos2(12.0, 12.0))
                .show(root.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        for line in lines {
                            ui.monospace(line);
                        }
                    });
                });
        });
        let jobs = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                self.renderer.update_texture(device, queue, *id, delta);
            }
        }
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point: output.pixels_per_point,
        };
        let extra = self
            .renderer
            .update_buffers(device, queue, encoder, &jobs, &screen);
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
            self.renderer.render(&mut pass, &jobs, &screen);
        }
        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        extra
    }
}
