//! Full-screen photo viewer with Live Photo and video playback.

use egui::{
    Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, RichText, Sense, TextureHandle, Ui, Vec2, pos2, vec2,
};

use crate::library::{Item, Kind, Library};
use crate::media::Media;
use crate::theme;
use crate::video::{self, Player};

pub enum Action {
    None,
    Close,
    ShowOnMap(u32),
}

struct Playback {
    key: u64,
    player: Player,
    texture: Option<TextureHandle>,
}

/// How far the photo is zoomed in: `scale` 1 fits it to the window, and
/// `offset` moves its centre away from the window's centre, in points.
#[derive(Clone, Copy)]
struct Zoom {
    scale: f32,
    offset: Vec2,
}

impl Zoom {
    const FIT: Self = Self { scale: 1.0, offset: Vec2::ZERO };

    fn zoomed(&self) -> bool {
        self.scale > 1.001
    }

    /// Zooms to `scale`, keeping the point under `anchor` where it is.
    fn zoom_about(&mut self, area: Rect, anchor: Pos2, scale: f32) {
        let centre = area.center() + self.offset;
        self.offset = anchor - (anchor - centre) * (scale / self.scale) - area.center();
        self.scale = scale;
    }

    /// Keeps a photo of `size` covering the window wherever it's bigger.
    fn clamp(&mut self, area: Rect, size: Vec2) {
        let room = ((size - area.size()) / 2.0).max(Vec2::ZERO);
        self.offset = self.offset.clamp(-room, room);
    }
}

pub struct Viewer {
    pub items: Vec<u32>,
    pub pos: usize,
    playback: Option<Playback>,
    place: Option<(u32, String)>,
    error: Option<String>,
    zoom: Zoom,
}

impl Viewer {
    pub fn new(items: Vec<u32>, pos: usize) -> Self {
        Self { items, pos, playback: None, place: None, error: None, zoom: Zoom::FIT }
    }

    fn go(&mut self, delta: isize) {
        let n = self.items.len() as isize;
        let next = (self.pos as isize + delta).clamp(0, n - 1) as usize;
        if next != self.pos {
            self.pos = next;
            self.playback = None;
            self.error = None;
            self.zoom = Zoom::FIT;
        }
    }

    fn play(&mut self, item: &Item, path: &std::path::Path, shown: [usize; 2], ctx: &egui::Context) {
        // Decode at most ~1280 px on the long edge; plenty for a 3 s clip.
        let s = (1280.0 / shown[0].max(shown[1]).max(1) as f32).min(1.0);
        let even = |v: usize| ((v as f32 * s) as usize / 2 * 2).max(2);
        match Player::start(path, [even(shown[0]), even(shown[1])], ctx.clone()) {
            Ok(player) => self.playback = Some(Playback { key: item.key, player, texture: None }),
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, lib: &Library, media: &mut Media) -> Action {
        let idx = self.items[self.pos];
        let item = &lib.items[idx as usize];
        let ctx = ui.ctx().clone();
        // Space and the arrow keys belong to the viewer; don't let a button
        // clicked earlier keep keyboard focus and get activated by them.
        if let Some(id) = ui.memory(|m| m.focused()) {
            ui.memory_mut(|m| m.surrender_focus(id));
        }
        let screen = ui.max_rect();
        ui.painter().rect_filled(screen, 0.0, theme::BG_DARK);

        // Long edge of the decoded full image, in physical pixels.
        let max_dim =
            ((screen.width().max(screen.height()) * ctx.pixels_per_point()) as u32 / 512 * 512).clamp(1024, 4096);
        let full = media.full(item, 0.0, max_dim);
        let thumb = media.thumb(item, 0.0);
        for (i, d) in [1isize, -1, 2].into_iter().enumerate() {
            let j = self.pos as isize + d;
            if j >= 0 && (j as usize) < self.items.len() {
                let other = &lib.items[self.items[j as usize] as usize];
                media.full(other, 1.0 + i as f32, max_dim);
                media.thumb(other, 1.0 + i as f32);
            }
        }

        // Playback frames.
        let mut finished = false;
        if let Some(pb) = &mut self.playback {
            if pb.key != item.key {
                finished = true;
            } else {
                if let Some(frame) = pb.player.take_frame() {
                    match &mut pb.texture {
                        Some(t) => t.set(frame, egui::TextureOptions::LINEAR),
                        None => pb.texture = Some(ctx.load_texture("live", frame, egui::TextureOptions::LINEAR)),
                    }
                }
                finished = pb.player.finished();
            }
        }
        if finished {
            self.playback = None;
        }

        let top_h = 56.0;
        let bottom_h = 64.0;
        let area = Rect::from_min_max(screen.min + vec2(0.0, top_h), screen.max - vec2(0.0, bottom_h)).shrink(6.0);

        // Photos zoom in to their full resolution: scroll, pinch or +/-,
        // double-click for 100%, drag to look around.
        let zoomable = item.kind == Kind::Photo && self.playback.is_none();
        if !zoomable {
            self.zoom = Zoom::FIT;
        }
        let original =
            if self.zoom.zoomed() { media.original(item, ctx.input(|i| i.max_texture_side) as u32) } else { None };
        let ppp = ctx.pixels_per_point();
        // Zoom that shows the photo at 100%, and the current percentage.
        let mut actual_size = None;
        let mut percent = None;

        // The image itself.
        let shown =
            self.playback.as_ref().and_then(|p| p.texture.clone()).or_else(|| full.clone()).or_else(|| thumb.clone());
        let mut shown_size = [item.width.max(1) as usize, item.height.max(1) as usize];
        if let Some(tex) = &shown {
            shown_size = tex.size();
            let tex_size = vec2(tex.size()[0] as f32, tex.size()[1] as f32);
            let aspect = tex_size.x / tex_size.y;
            let mut fit = area.size();
            if fit.x / fit.y > aspect {
                fit.x = fit.y * aspect;
            } else {
                fit.y = fit.x / aspect;
            }
            // Don't blow small images up beyond 2x.
            let natural = tex_size / ppp;
            if full.is_some() && fit.x > natural.x * 2.0 {
                fit = natural * 2.0;
            }
            let pixels = original.as_ref().map(|t| t.size()[0] as f32).or(display_size(item).map(|s| s.x));
            let pixels = pixels.unwrap_or(tex_size.x);
            // Screen pixels per photo pixel, at scale 1.
            let fit_px = fit.x * ppp / pixels;
            if zoomable {
                let one_to_one = if fit_px < 0.95 { 1.0 / fit_px } else { 2.0 };
                let max_scale = (4.0 / fit_px).max(1.0);
                actual_size = Some(one_to_one.min(max_scale));
                let resp = ui.interact(area, ui.scope_id().with("photo"), Sense::click_and_drag());
                let anchor = resp.hover_pos().unwrap_or(area.center());
                let (scroll, pinch, zoom_in, zoom_out) = ui.input(|i| {
                    let zoom_in = i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals);
                    (i.smooth_scroll_delta.y, i.zoom_delta(), zoom_in, i.key_pressed(Key::Minus))
                });
                let mut factor = if resp.hovered() { (scroll / 180.0).exp2() * pinch } else { 1.0 };
                factor *= if zoom_in {
                    2.0
                } else if zoom_out {
                    0.5
                } else {
                    1.0
                };
                if (factor - 1.0).abs() > 1e-4 {
                    self.zoom.zoom_about(area, anchor, (self.zoom.scale * factor).clamp(1.0, max_scale));
                }
                if resp.double_clicked() {
                    if self.zoom.zoomed() {
                        self.zoom = Zoom::FIT;
                    } else {
                        self.zoom.zoom_about(area, anchor, one_to_one.min(max_scale));
                    }
                }
                if resp.dragged() {
                    self.zoom.offset += resp.drag_delta();
                }
                if self.zoom.zoomed() {
                    resp.on_hover_cursor(if ui.input(|i| i.pointer.primary_down()) {
                        CursorIcon::Grabbing
                    } else {
                        CursorIcon::Grab
                    });
                }
            }
            let size = fit * self.zoom.scale;
            self.zoom.clamp(area, size);
            let px = fit_px * self.zoom.scale;
            if self.zoom.zoomed() {
                percent = Some(px * 100.0);
            }
            // Without mipmaps a texture can't be shrunk much more than 2x and
            // stay smooth, so the screen-sized image is used until then.
            let tex = match &original {
                Some(o) if px >= 0.5 => o,
                _ => tex,
            };
            let r = Rect::from_center_size(area.center() + self.zoom.offset, size);
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            let painter = ui.painter().with_clip_rect(area.expand(6.0));
            theme::frame(&painter, r.expand(4.0), theme::BG_DARK);
            painter.image(tex.id(), r, uv, Color32::WHITE);
            if self.zoom.zoomed() && px >= 0.5 && original.is_none() && !media.original_failed(item) {
                let r = Rect::from_center_size(area.right_top() + vec2(-22.0, 22.0), vec2(20.0, 20.0));
                ui.put(r, egui::Spinner::new().size(20.0).color(theme::MUTED));
            }
        }
        if full.is_none() && !media.full_failed(item) {
            let r = Rect::from_center_size(area.center(), vec2(28.0, 28.0));
            ui.put(r, egui::Spinner::new().size(28.0).color(theme::MUTED));
        } else if full.is_none() && thumb.is_none() {
            ui.painter().text(
                area.center(),
                Align2::CENTER_CENTER,
                "This file could not be decoded",
                FontId::proportional(16.0),
                theme::MUTED,
            );
        }

        let mut action = Action::None;

        // Side arrows.
        for (dir, label) in [(-1isize, "‹"), (1, "›")] {
            let enabled = if dir < 0 { self.pos > 0 } else { self.pos + 1 < self.items.len() };
            if !enabled {
                continue;
            }
            let x = if dir < 0 { area.left() } else { area.right() - 64.0 };
            let r = Rect::from_min_size(pos2(x, area.top()), vec2(64.0, area.height()));
            let resp = ui.interact(r, ui.scope_id().with(("nav", dir)), Sense::click());
            if resp.hovered() {
                let c = Rect::from_center_size(r.center(), vec2(44.0, 44.0));
                theme::frame(ui.painter(), c, theme::BG);
                ui.painter().text(
                    c.center() - vec2(0.0, 2.0),
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(30.0),
                    theme::FG,
                );
            }
            if resp.clicked() {
                self.go(dir);
            }
        }

        // Top bar.
        let top = Rect::from_min_size(screen.min, vec2(screen.width(), top_h));
        ui.painter().rect_filled(top, 0.0, theme::BG);
        theme::rule_h(ui.painter(), top.x_range(), top.bottom(), false);
        let mut bar = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(top.shrink2(vec2(12.0, 8.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        if bar.add(egui::Button::new(RichText::new("✕  Close").size(14.0))).clicked() {
            action = Action::Close;
        }
        let (title, subtitle) = match item.taken_local() {
            Some(t) => {
                (t.format("%A, %-d %B %Y").to_string(), format!("{}   ·   {}", t.format("%H:%M"), item.file_name()))
            }
            None => (item.file_name(), String::new()),
        };
        ui.painter().text(
            top.center() - vec2(0.0, 9.0),
            Align2::CENTER_CENTER,
            title,
            theme::blackletter(21.0),
            theme::FG,
        );
        ui.painter().text(
            top.center() + vec2(0.0, 12.0),
            Align2::CENTER_CENTER,
            subtitle,
            FontId::proportional(12.0),
            theme::MUTED,
        );
        let counter = format!("{} of {}", self.pos + 1, self.items.len());
        ui.painter().text(
            top.right_center() - vec2(16.0, 2.0),
            Align2::RIGHT_CENTER,
            match percent {
                Some(p) => format!("{p:.0}%   ·   {counter}"),
                None => counter,
            },
            FontId::proportional(13.0),
            theme::MUTED,
        );

        // Bottom bar.
        let bottom = Rect::from_min_max(pos2(screen.left(), screen.bottom() - bottom_h), screen.max);
        ui.painter().rect_filled(bottom, 0.0, theme::BG);
        theme::rule_h(ui.painter(), bottom.x_range(), bottom.top(), true);
        if self.place.as_ref().is_none_or(|(i, _)| *i != idx) {
            self.place = Some((idx, crate::app::place_name(item.lat, item.lon)));
        }
        let place = &self.place.as_ref().unwrap().1;
        let mut details = Vec::new();
        if let Some(c) = &item.caption {
            details.push(format!("“{c}”"));
        }
        details.push(format_coords(item.lat, item.lon));
        if let Some(m) = &item.model {
            details.push(m.clone());
        }
        if let Some(s) = display_size(item) {
            details.push(format!("{} × {}", s.x, s.y));
        }
        if let Some(d) = item.duration.filter(|_| item.kind == Kind::Video) {
            details.push(format_duration(d));
        }
        if item.edited.is_some() {
            details.push("Edited".into());
        }
        let left = bottom.left_center() + vec2(16.0, 0.0);
        let r =
            ui.painter().text(left - vec2(0.0, 8.0), Align2::LEFT_CENTER, place, theme::blackletter(19.0), theme::FG);
        if item.favorite {
            let pos = r.right_center() + vec2(8.0, 0.0);
            ui.painter().text(pos, Align2::LEFT_CENTER, "♥", FontId::proportional(15.0), theme::BRASS);
        }
        ui.painter().text(
            left + vec2(0.0, 14.0),
            Align2::LEFT_CENTER,
            details.join("   ·   "),
            FontId::proportional(12.0),
            theme::MUTED,
        );
        if let Some(e) = &self.error {
            ui.painter().text(
                bottom.center(),
                Align2::CENTER_CENTER,
                e,
                FontId::proportional(12.0),
                ui.visuals().error_fg_color,
            );
        }

        let mut buttons = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bottom.shrink2(vec2(16.0, 14.0)))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        let playing = self.playback.is_some();
        if buttons.button("Open…").on_hover_text("Open in the default app (with sound)").clicked() {
            video::open_external(item.image_path());
        }
        if buttons.button("Show on Map").clicked() {
            action = Action::ShowOnMap(idx);
        }
        if let Some(scale) = actual_size {
            let label = if self.zoom.zoomed() { "Fit" } else { "Full size" };
            let hint = "Double-click the photo, or scroll to zoom; drag to look around";
            if buttons.button(label).on_hover_text(hint).clicked() {
                if self.zoom.zoomed() {
                    self.zoom = Zoom::FIT;
                } else {
                    self.zoom.zoom_about(area, area.center(), scale);
                }
                ctx.request_repaint();
            }
        }
        let mut toggle = ui.input(|i| i.key_pressed(Key::Space));
        let motion = match item.kind {
            Kind::Video => Some(item.path.as_path()),
            Kind::Photo => item.live.as_deref(),
        };
        if let Some(path) = motion {
            let label = match (item.kind, playing) {
                (_, true) => "■  Stop",
                (Kind::Video, false) => "Play",
                (Kind::Photo, false) => "◉  LIVE",
            };
            // Selected (playing) buttons get the selection colours instead.
            let text = if playing { RichText::new(label) } else { RichText::new(label).color(theme::BRASS) };
            let btn = egui::Button::new(text).selected(playing);
            if buttons.add(btn).on_hover_text("Space").clicked() {
                toggle = true;
            }
            if toggle {
                if playing {
                    self.playback = None;
                } else {
                    self.zoom = Zoom::FIT;
                    self.play(item, path, shown_size, &ctx);
                }
            }
        }

        // Esc steps out of a zoom before it closes the viewer.
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            if self.zoom.zoomed() {
                self.zoom = Zoom::FIT;
            } else {
                action = Action::Close;
            }
        }
        let (left_key, right_key) = ui.input(|i| (i.key_pressed(Key::ArrowLeft), i.key_pressed(Key::ArrowRight)));
        if left_key {
            self.go(-1);
        }
        if right_key {
            self.go(1);
        }
        action
    }
}

/// The photo's size in pixels as shown: EXIF stores sensor dimensions, and
/// orientations 5–8 are rotated 90°.
fn display_size(item: &Item) -> Option<Vec2> {
    let (w, h) = (item.width as f32, item.height as f32);
    (item.width > 0 && item.height > 0).then(|| if item.orientation >= 5 { vec2(h, w) } else { vec2(w, h) })
}

pub fn format_coords(lat: f64, lon: f64) -> String {
    format!(
        "{:.5}° {}, {:.5}° {}",
        lat.abs(),
        if lat >= 0.0 { "N" } else { "S" },
        lon.abs(),
        if lon >= 0.0 { "E" } else { "W" }
    )
}

pub fn format_duration(secs: f32) -> String {
    let s = secs.round() as u32;
    format!("{}:{:02}", s / 60, s % 60)
}
