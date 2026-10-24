//! The slippy map: view state, input handling, tile drawing and labels.

use egui::{Align2, Color32, FontId, Pos2, Rect, Response, Ui, Vec2, pos2, vec2};

use crate::tiles::{TileKey, TileLayer};
use crate::{basemap, theme};

pub const MAX_ZOOM: f64 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// Centre in normalised Web Mercator coordinates.
    pub cx: f64,
    pub cy: f64,
    pub zoom: f64,
}

struct Flight {
    from: View,
    to: View,
    start: f64,
    duration: f64,
}

pub struct MapView {
    pub view: View,
    flight: Option<Flight>,
    /// Pan velocity in points per second, for kinetic scrolling.
    velocity: Vec2,
    /// Map area and window size seen last frame.
    rect: Option<(Rect, Vec2)>,
}

impl MapView {
    pub fn new(view: View) -> Self {
        Self { view, flight: None, velocity: Vec2::ZERO, rect: None }
    }

    /// Call once per frame with the map's screen area. When a side panel
    /// opens or closes, the map stays put instead of re-centring; when the
    /// whole window is resized it keeps its centre.
    pub fn set_rect(&mut self, rect: Rect, window: Vec2) {
        if let Some((old, old_window)) = self.rect.replace((rect, window)) {
            let shift = rect.center() - old.center();
            if shift != Vec2::ZERO && window == old_window {
                let w = self.world();
                let (dx, dy) = (shift.x as f64 / w, shift.y as f64 / w);
                for v in [Some(&mut self.view), self.flight.as_mut().map(|f| &mut f.to)].into_iter().flatten() {
                    v.cx += dx;
                    v.cy += dy;
                }
                if let Some(f) = &mut self.flight {
                    f.from.cx += dx;
                    f.from.cy += dy;
                }
            }
        }
    }

    pub fn world(&self) -> f64 {
        256.0 * self.view.zoom.exp2()
    }

    /// Screen position of a world point (using the copy nearest the centre).
    pub fn project(&self, rect: Rect, x: f64, y: f64) -> Pos2 {
        let w = self.world();
        let mut dx = x - self.view.cx;
        dx -= dx.round();
        let c = rect.center();
        pos2(c.x + (dx * w) as f32, c.y + ((y - self.view.cy) * w) as f32)
    }

    /// World point under a screen position (x is not wrapped).
    pub fn unproject(&self, rect: Rect, p: Pos2) -> (f64, f64) {
        let w = self.world();
        let c = rect.center();
        (self.view.cx + (p.x - c.x) as f64 / w, self.view.cy + (p.y - c.y) as f64 / w)
    }

    fn min_zoom(rect: Rect) -> f64 {
        (rect.height() as f64 / 256.0).log2().max(0.0)
    }

    fn clamp(&mut self, rect: Rect) {
        let v = &mut self.view;
        v.zoom = v.zoom.clamp(Self::min_zoom(rect), MAX_ZOOM);
        v.cx = v.cx.rem_euclid(1.0);
        let half = rect.height() as f64 / 2.0 / (256.0 * v.zoom.exp2());
        v.cy = if half >= 0.5 { 0.5 } else { v.cy.clamp(half, 1.0 - half) };
    }

    /// Zooms by `dz` levels keeping the world point under `anchor` fixed.
    fn zoom_at(&mut self, rect: Rect, anchor: Pos2, dz: f64) {
        let (wx, wy) = self.unproject(rect, anchor);
        self.view.zoom = (self.view.zoom + dz).clamp(Self::min_zoom(rect), MAX_ZOOM);
        let w = self.world();
        let c = rect.center();
        self.view.cx = wx - (anchor.x - c.x) as f64 / w;
        self.view.cy = wy - (anchor.y - c.y) as f64 / w;
    }

    pub fn fly_to(&mut self, ui: &Ui, to: View) {
        let now = ui.input(|i| i.time);
        let mut to = to;
        // Take the short way round the antimeridian.
        to.cx = self.view.cx + (to.cx - self.view.cx - (to.cx - self.view.cx).round());
        let dz = (to.zoom - self.view.zoom).abs();
        let duration = (0.35 + dz * 0.06).min(0.9);
        self.flight = Some(Flight { from: self.view, to, start: now, duration });
        self.velocity = Vec2::ZERO;
        ui.ctx().request_repaint();
    }

    /// View that fits a bounding box (normalised coordinates) into `rect`.
    pub fn fit(rect: Rect, (x0, y0, x1, y1): (f64, f64, f64, f64), padding: f32, max_zoom: f64) -> View {
        let w = (x1 - x0).max(1e-9);
        let h = (y1 - y0).max(1e-9);
        let avail = (rect.size() - Vec2::splat(padding * 2.0)).max(Vec2::splat(64.0));
        let zx = (avail.x as f64 / (256.0 * w)).log2();
        let zy = (avail.y as f64 / (256.0 * h)).log2();
        View { cx: (x0 + x1) / 2.0, cy: (y0 + y1) / 2.0, zoom: zx.min(zy).min(max_zoom) }
    }

    pub fn handle_input(&mut self, ui: &Ui, rect: Rect, response: &Response) {
        let (dt, now) = ui.input(|i| (i.stable_dt.min(0.1) as f64, i.time));

        if let Some(f) = &self.flight {
            let t = ((now - f.start) / f.duration).clamp(0.0, 1.0);
            let e = 1.0 - (1.0 - t).powi(3);
            let lerp = |a: f64, b: f64| a + (b - a) * e;
            self.view =
                View { cx: lerp(f.from.cx, f.to.cx), cy: lerp(f.from.cy, f.to.cy), zoom: lerp(f.from.zoom, f.to.zoom) };
            if t >= 1.0 {
                self.flight = None;
            }
            ui.ctx().request_repaint();
        }

        if response.dragged() {
            self.flight = None;
            let d = response.drag_delta();
            let w = self.world();
            self.view.cx -= d.x as f64 / w;
            self.view.cy -= d.y as f64 / w;
            if dt > 0.0 {
                let v = d / dt as f32;
                self.velocity = self.velocity * 0.6 + v * 0.4;
            }
        } else if response.drag_stopped() {
            let still = ui.input(|i| i.pointer.velocity().length() < 30.0);
            if still {
                self.velocity = Vec2::ZERO;
            }
        } else if self.velocity.length() > 8.0 {
            let w = self.world();
            self.view.cx -= (self.velocity.x as f64 * dt) / w;
            self.view.cy -= (self.velocity.y as f64 * dt) / w;
            self.velocity *= (-dt * 5.0).exp() as f32;
            ui.ctx().request_repaint();
        } else {
            self.velocity = Vec2::ZERO;
        }

        if response.hovered() {
            let (scroll, pinch, pointer) =
                ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.pointer.hover_pos()));
            let dz = scroll as f64 / 180.0 + (pinch as f64).log2();
            if dz.abs() > 1e-4 {
                self.flight = None;
                self.zoom_at(rect, pointer.unwrap_or(rect.center()), dz);
            }
        }
        if response.double_clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            let (x, y) = self.unproject(rect, p);
            let zoom = (self.view.zoom + 1.0).min(MAX_ZOOM);
            // Keep the clicked point under the cursor once zoomed in.
            let w = 256.0 * zoom.exp2();
            let c = rect.center();
            let to = View { cx: x - (p.x - c.x) as f64 / w, cy: y - (p.y - c.y) as f64 / w, zoom };
            self.fly_to(ui, to);
        }
        if response.hovered() || response.has_focus() {
            let (zin, zout) = ui.input(|i| {
                (i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals), i.key_pressed(egui::Key::Minus))
            });
            let dz = if zin {
                1.0
            } else if zout {
                -1.0
            } else {
                0.0
            };
            if dz != 0.0 {
                let to = View { zoom: (self.view.zoom + dz).clamp(Self::min_zoom(rect), MAX_ZOOM), ..self.view };
                self.fly_to(ui, to);
            }
        }
        self.clamp(rect);
        if let Some(f) = &mut self.flight {
            f.to.zoom = f.to.zoom.clamp(Self::min_zoom(rect), MAX_ZOOM);
        }
    }

    pub fn zoom_buttons(&mut self, ui: &mut Ui, rect: Rect) {
        let size = vec2(30.0, 30.0);
        let base = rect.right_bottom() - vec2(18.0 + size.x, 36.0 + size.y * 2.0);
        let outer = Rect::from_min_size(base, vec2(size.x, size.y * 2.0)).expand(4.0);
        theme::frame(ui.painter(), outer, theme::BG);
        for (i, (label, dz)) in [("+", 1.0), ("−", -1.0)].into_iter().enumerate() {
            let r = Rect::from_min_size(base + vec2(0.0, i as f32 * size.y), size);
            let resp = ui.interact(r, ui.scope_id().with(("zoom", i)), egui::Sense::click());
            let p = ui.painter();
            if resp.hovered() {
                // An inset square, like the waybar hover highlight.
                p.rect_filled(r.shrink(3.0), 0.0, theme::BAND);
            }
            p.text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(20.0), theme::FG);
            if resp.clicked() {
                let to = View { zoom: (self.view.zoom + dz).clamp(Self::min_zoom(rect), MAX_ZOOM), ..self.view };
                self.fly_to(ui, to);
            }
        }
        ui.painter().hline(base.x..=base.x + size.x, base.y + size.y, (1.0, theme::BAND));
    }

    /// Draws one tile layer. Missing tiles are requested and, meanwhile,
    /// stood in for by a scaled-up ancestor when one is loaded.
    pub fn draw_tiles(&self, ui: &Ui, rect: Rect, layer: &mut TileLayer) {
        if self.view.zoom < layer.min_zoom as f64 - 0.5 {
            return;
        }
        let ppp = ui.ctx().pixels_per_point();
        let px = if layer.scalable { if ppp > 1.25 { 512 } else { 256 } } else { 0 };
        let zt = (self.view.zoom.round() as i64).clamp(layer.min_zoom as i64, layer.max_zoom as i64) as u8;
        let n = 1i64 << zt;
        let world = self.world();
        let tile = world / n as f64;
        let left = self.view.cx * world - rect.width() as f64 / 2.0;
        let top = self.view.cy * world - rect.height() as f64 / 2.0;
        let (tx0, tx1) = ((left / tile).floor() as i64, ((left + rect.width() as f64) / tile).floor() as i64);
        let ty0 = ((top / tile).floor() as i64).max(0);
        let ty1 = (((top + rect.height() as f64) / tile).floor() as i64).min(n - 1);
        let centre = ((left + rect.width() as f64 / 2.0) / tile, (top + rect.height() as f64 / 2.0) / tile);

        let painter = ui.painter_at(rect);
        let full = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
        for ty in ty0..=ty1 {
            for txi in tx0..=tx1 {
                let x = txi.rem_euclid(n) as u32;
                let min =
                    pos2(rect.left() + (txi as f64 * tile - left) as f32, rect.top() + (ty as f64 * tile - top) as f32);
                let max = pos2(
                    rect.left() + ((txi + 1) as f64 * tile - left) as f32,
                    rect.top() + ((ty + 1) as f64 * tile - top) as f32,
                );
                let screen = Rect::from_min_max(min, max);
                let key = TileKey { z: zt, x, y: ty as u32, px };
                if let Some(tex) = layer.cache.get(&key) {
                    painter.image(tex.id(), screen, full, Color32::WHITE);
                    continue;
                }
                let dist = ((txi as f64 + 0.5 - centre.0).powi(2) + (ty as f64 + 0.5 - centre.1).powi(2)) as f32;
                layer.cache.request(key, dist, || key);
                for up in 1..=zt.min(10) {
                    let parent = TileKey { z: zt - up, x: x >> up, y: (ty as u32) >> up, px };
                    if let Some(tex) = layer.cache.get(&parent) {
                        let s = 1.0 / (1u32 << up) as f32;
                        let mask = (1u32 << up) - 1;
                        let uv =
                            Rect::from_min_size(pos2((x & mask) as f32 * s, (ty as u32 & mask) as f32 * s), vec2(s, s));
                        painter.image(tex.id(), screen, uv, Color32::WHITE);
                        break;
                    }
                }
            }
        }
    }

    /// Whether `layer` has a loaded tile covering world point (x, y) at the
    /// current zoom.
    pub fn covered_by(&self, layer: &TileLayer, x: f64, y: f64) -> bool {
        if self.view.zoom < layer.min_zoom as f64 - 0.5 {
            return false;
        }
        let zt = (self.view.zoom.round() as i64).clamp(layer.min_zoom as i64, layer.max_zoom as i64) as u8;
        let n = (1u64 << zt) as f64;
        let key = TileKey { z: zt, x: (x.rem_euclid(1.0) * n) as u32, y: (y * n).min(n - 1.0) as u32, px: 0 };
        layer.cache.contains(&key)
    }

    /// Country and city names from the built-in basemap.
    /// Labels avoid `obstacles` (photo markers) and each other.
    pub fn draw_labels(&self, ui: &Ui, rect: Rect, obstacles: &[Rect], hide: impl Fn(f64, f64) -> bool) {
        let painter = ui.painter_at(rect);
        let zoom = self.view.zoom as f32;
        let mut placed: Vec<Rect> = obstacles.to_vec();
        let halo = theme::BG_DARK.gamma_multiply(0.85);

        let mut put = |text: String, pos: Pos2, font: FontId, color: Color32, dot: bool| -> bool {
            let galley = painter.layout_no_wrap(text, font, color);
            let offset = if dot { vec2(6.0, -galley.size().y / 2.0) } else { -galley.size() / 2.0 };
            let r = Rect::from_min_size(pos + offset, galley.size());
            let pad = r.expand2(vec2(6.0, 2.0));
            if !rect.contains_rect(r) || placed.iter().any(|p| p.intersects(pad)) {
                return false;
            }
            placed.push(pad);
            for d in
                [vec2(-1.0, 0.0), vec2(1.0, 0.0), vec2(0.0, -1.0), vec2(0.0, 1.0), vec2(1.0, 1.0), vec2(-1.0, -1.0)]
            {
                painter.galley_with_override_text_color(r.min + d, galley.clone(), halo);
            }
            painter.galley(r.min, galley, color);
            if dot {
                painter.circle(pos, 2.6, theme::FG, (1.2, theme::BG_DARK));
            }
            true
        };

        let map = basemap::get();
        let mut n = 0;
        for c in &map.cities {
            if n >= 120 {
                break;
            }
            if zoom + 1.0 < c.min_zoom || (zoom < 4.0 && !c.capital) {
                continue;
            }
            let pos = self.project(rect, c.x as f64, c.y as f64);
            if !rect.contains(pos) || hide(c.x as f64, c.y as f64) {
                continue;
            }
            let size = if c.capital || c.pop > 2_000_000 {
                13.0
            } else if c.pop > 200_000 {
                12.0
            } else {
                11.0
            };
            if put(c.name.clone(), pos, FontId::proportional(size), theme::FG, true) {
                n += 1;
            }
        }
        if zoom < 7.0 {
            for c in &map.countries {
                if zoom + 1.0 < c.min_label || zoom > c.max_label {
                    continue;
                }
                let pos = self.project(rect, c.x as f64, c.y as f64);
                if rect.contains(pos) && !hide(c.x as f64, c.y as f64) {
                    put(c.name.clone(), pos, theme::blackletter(17.0), theme::MUTED, false);
                }
            }
        }
    }
}
