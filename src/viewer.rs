//! Full-screen photo viewer with Live Photo and video playback.

use egui::{Align2, Color32, FontId, Key, Rect, RichText, Sense, TextureHandle, Ui, pos2, vec2};

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

pub struct Viewer {
    pub items: Vec<u32>,
    pub pos: usize,
    playback: Option<Playback>,
    place: Option<(u32, String)>,
    error: Option<String>,
}

impl Viewer {
    pub fn new(items: Vec<u32>, pos: usize) -> Self {
        Self { items, pos, playback: None, place: None, error: None }
    }

    fn go(&mut self, delta: isize) {
        let n = self.items.len() as isize;
        let next = (self.pos as isize + delta).clamp(0, n - 1) as usize;
        if next != self.pos {
            self.pos = next;
            self.playback = None;
            self.error = None;
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

        // The image itself.
        let shown =
            self.playback.as_ref().and_then(|p| p.texture.clone()).or_else(|| full.clone()).or_else(|| thumb.clone());
        let mut shown_size = [item.width.max(1) as usize, item.height.max(1) as usize];
        if let Some(tex) = &shown {
            shown_size = tex.size();
            let aspect = tex.size()[0] as f32 / tex.size()[1] as f32;
            let mut size = area.size();
            if size.x / size.y > aspect {
                size.x = size.y * aspect;
            } else {
                size.y = size.x / aspect;
            }
            // Don't blow small images up beyond 2x.
            let natural = vec2(tex.size()[0] as f32, tex.size()[1] as f32) / ctx.pixels_per_point();
            if full.is_some() && size.x > natural.x * 2.0 {
                size = natural * 2.0;
            }
            let r = Rect::from_center_size(area.center(), size);
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            theme::frame(ui.painter(), r.expand(4.0), theme::BG_DARK);
            ui.painter().image(tex.id(), r, uv, Color32::WHITE);
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
        ui.painter().text(
            top.right_center() - vec2(16.0, 2.0),
            Align2::RIGHT_CENTER,
            format!("{} of {}", self.pos + 1, self.items.len()),
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
        if item.width > 0 {
            // EXIF stores sensor dimensions; orientations 5–8 are rotated 90°.
            let (w, h) = if item.orientation >= 5 { (item.height, item.width) } else { (item.width, item.height) };
            details.push(format!("{w} × {h}"));
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
                    self.play(item, path, shown_size, &ctx);
                }
            }
        }

        ui.input(|i| {
            if i.key_pressed(Key::Escape) {
                action = Action::Close;
            }
        });
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
