//! The application window: top bar, map with photo markers, the grid of
//! photos for a selected place, and the full-screen viewer.

use std::path::PathBuf;

use crossbeam_channel::Receiver;
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, pos2, vec2};
use serde::{Deserialize, Serialize};

use crate::basemap;
use crate::cluster::Clusters;
use crate::library::{self, Item, Kind, Library};
use crate::map::{MapView, View};
use crate::media::{self, Media};
use crate::theme;
use crate::tiles::TileLayer;
use crate::viewer::{self, Viewer};

/// Marker edge length in points.
const MARKER: f32 = 50.0;
/// Minimum distance between marker centres before they merge, in points.
const CLUSTER_RADIUS: f64 = 60.0;

#[derive(Serialize, Deserialize, Default)]
struct Config {
    root: Option<PathBuf>,
    tiles: Option<PathBuf>,
    view: Option<(f64, f64, f64)>,
}

impl Config {
    fn path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("mappa-mundi").join("config.json")
    }

    fn load() -> Self {
        std::fs::read(Self::path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn save(&self) {
        let path = Self::path();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        if let Err(e) = std::fs::write(&path, serde_json::to_vec_pretty(self).unwrap()) {
            eprintln!("mappa-mundi: saving {}: {e}", path.display());
        }
    }
}

pub struct Options {
    pub root: Option<PathBuf>,
    pub tiles: Option<PathBuf>,
}

enum ScanMsg {
    Progress(usize, usize),
    Done(Box<(Library, Clusters)>),
    Failed(String),
}

struct Selection {
    members: Vec<u32>,
    title: String,
    subtitle: String,
}

pub struct MappaMundiApp {
    config: Config,
    library: Option<Library>,
    clusters: Option<Clusters>,
    scan: Option<Receiver<ScanMsg>>,
    progress: (usize, usize),
    message: Option<String>,
    map: MapView,
    fit_on_load: bool,
    basemap: TileLayer,
    overlay: Option<TileLayer>,
    media: Media,
    selection: Option<Selection>,
    viewer: Option<Viewer>,
    folder_dialog: Option<Receiver<Option<PathBuf>>>,
}

impl MappaMundiApp {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options) -> Self {
        let ctx = &cc.egui_ctx;
        theme::apply(ctx);
        let mut config = Config::load();
        if opts.root.is_some() {
            config.root = opts.root;
        }
        if opts.tiles.is_some() {
            config.tiles = opts.tiles;
        }
        let mut message = None;
        let overlay = config.tiles.as_ref().and_then(|p| match TileLayer::open(p, ctx) {
            Ok(layer) => Some(layer),
            Err(e) => {
                message = Some(format!("Could not open map tiles: {e}"));
                None
            }
        });
        if overlay.is_none() {
            // Don't keep complaining about the same file on every launch.
            config.tiles = None;
        }
        let view = config.view.map(|(cx, cy, zoom)| View { cx, cy, zoom });
        let mut app = Self {
            map: MapView::new(view.unwrap_or(View { cx: 0.5, cy: 0.4, zoom: 1.5 })),
            fit_on_load: view.is_none(),
            library: None,
            clusters: None,
            scan: None,
            progress: (0, 0),
            message,
            basemap: TileLayer::basemap(ctx),
            overlay,
            media: Media::new(ctx),
            selection: None,
            viewer: None,
            folder_dialog: None,
            config,
        };
        if let Some(root) = app.config.root.clone() {
            app.start_scan(root, ctx);
        }
        app
    }

    fn start_scan(&mut self, root: PathBuf, ctx: &egui::Context) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let ctx = ctx.clone();
        self.progress = (0, 0);
        self.scan = Some(rx);
        std::thread::Builder::new()
            .name("scan".into())
            .spawn(move || {
                let progress = |done, total| {
                    let _ = tx.send(ScanMsg::Progress(done, total));
                    ctx.request_repaint();
                };
                let msg = match library::scan(&root, progress) {
                    Ok(lib) => {
                        let clusters = Clusters::build(&lib.items, CLUSTER_RADIUS);
                        ScanMsg::Done(Box::new((lib, clusters)))
                    }
                    Err(e) => ScanMsg::Failed(format!("{e:#}")),
                };
                let _ = tx.send(msg);
                ctx.request_repaint();
            })
            .expect("spawning scan thread");
    }

    fn poll_background(&mut self, ui: &Ui) {
        if let Some(rx) = &self.scan {
            for msg in rx.try_iter().collect::<Vec<_>>() {
                match msg {
                    ScanMsg::Progress(d, t) => self.progress = (d, t),
                    ScanMsg::Done(result) => {
                        let (lib, clusters) = *result;
                        self.library = Some(lib);
                        self.clusters = Some(clusters);
                        self.scan = None;
                        // Item indices may have changed; drop anything referring to them.
                        self.selection = None;
                        self.viewer = None;
                        if self.fit_on_load {
                            self.fit_on_load = false;
                            self.show_all(ui, false);
                        }
                    }
                    ScanMsg::Failed(e) => {
                        self.message = Some(e);
                        self.scan = None;
                    }
                }
            }
        }
        if let Some(rx) = &self.folder_dialog
            && let Ok(choice) = rx.try_recv()
        {
            self.folder_dialog = None;
            if let Some(root) = choice {
                self.config.root = Some(root.clone());
                self.config.save();
                self.library = None;
                self.clusters = None;
                self.selection = None;
                self.fit_on_load = true;
                self.start_scan(root, ui.ctx());
            }
        }
    }

    fn choose_folder(&mut self, ctx: &egui::Context) {
        if self.folder_dialog.is_some() {
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        let ctx = ctx.clone();
        let start = self.config.root.clone().or_else(dirs::picture_dir);
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("Choose your photo library folder");
            if let Some(dir) = start {
                dialog = dialog.set_directory(dir);
            }
            let _ = tx.send(dialog.pick_folder());
            ctx.request_repaint();
        });
        self.folder_dialog = Some(rx);
    }

    fn show_all(&mut self, ui: &Ui, animate: bool) {
        let Some(lib) = &self.library else { return };
        let Some(b) = bounds(lib.items.iter()) else { return };
        let rect = ui.ctx().content_rect();
        let view = MapView::fit(rect, b, 70.0, 15.0);
        if animate {
            self.map.fly_to(ui, view);
        } else {
            self.map.view = view;
        }
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        let mut show_all = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.label(RichText::new("Mappa Mundi").font(theme::blackletter(24.0)).color(theme::BRASS));
            ui.separator();
            if ui.button("Open Folder…").clicked() {
                self.choose_folder(ui.ctx());
            }
            if let Some(root) = self.config.root.clone() {
                let name =
                    root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned());
                ui.label(RichText::new(name).weak()).on_hover_text(root.display().to_string());
                if self.scan.is_none() && ui.button("Rescan").on_hover_text("Look for new or changed files").clicked() {
                    self.start_scan(root, ui.ctx());
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.scan.is_some() {
                    let (done, total) = self.progress;
                    let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                    let text = if total == 0 {
                        "Finding files…".to_string()
                    } else {
                        format!("Reading {} / {}", group(done), group(total))
                    };
                    ui.add(egui::ProgressBar::new(frac).desired_width(220.0).text(text));
                    ui.spinner();
                } else if let Some(lib) = &self.library {
                    if !lib.items.is_empty() && ui.button("Show All").clicked() {
                        show_all = true;
                    }
                    let mut s = format!("{} on map", group(lib.items.len()));
                    if lib.live > 0 {
                        s += &format!("  ·  {} Live", group(lib.live));
                    }
                    if lib.videos > 0 {
                        s += &format!("  ·  {} videos", group(lib.videos));
                    }
                    if lib.without_location > 0 {
                        s += &format!("  ·  {} without location", group(lib.without_location));
                    }
                    ui.add(egui::Label::new(RichText::new(s).weak()).truncate());
                }
            });
        });
        if show_all {
            self.show_all(ui, true);
        }
    }

    fn map_ui(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        self.map.set_rect(rect, ui.ctx().content_rect().size());
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let o = basemap::OCEAN;
        painter.rect_filled(rect, 0.0, Color32::from_rgb(o[0], o[1], o[2]));

        // Work out which marker (if any) is under the pointer before the map
        // reacts, so clicks on markers don't also pan or zoom the map.
        let markers = self.visible_markers(rect);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let hovered =
            pointer.and_then(|p| markers.iter().rev().find(|m| m.rect.expand(3.0).contains(p)).map(|m| m.cluster));

        if hovered.is_some() && response.double_clicked() {
            if let (Some(c), Some(clusters)) = (hovered, &self.clusters) {
                let cl = &clusters.at_zoom(self.map.view.zoom)[c];
                let lib = self.library.as_ref().unwrap();
                if let Some(b) = bounds(cl.members.iter().map(|&i| &lib.items[i as usize])) {
                    let mut to = MapView::fit(rect, b, 90.0, 18.0);
                    // Always zoom in far enough for the group to split up.
                    to.zoom = to.zoom.max(self.map.view.zoom.floor() + 1.0);
                    self.map.fly_to(ui, to);
                }
            }
        } else {
            self.map.handle_input(ui, rect, &response);
        }

        self.map.draw_tiles(ui, rect, &mut self.basemap);
        if let Some(overlay) = &mut self.overlay {
            self.map.draw_tiles(ui, rect, overlay);
        }

        // Recompute after input so markers track the map exactly.
        let markers = self.visible_markers(rect);
        let obstacles: Vec<Rect> = markers.iter().map(|m| m.rect.expand(4.0)).collect();
        let overlay = self.overlay.as_ref();
        let map = &self.map;
        map.draw_labels(ui, rect, &obstacles, |x, y| overlay.is_some_and(|o| map.covered_by(o, x, y)));

        let hovered =
            pointer.and_then(|p| markers.iter().rev().find(|m| m.rect.expand(3.0).contains(p)).map(|m| m.cluster));
        if hovered.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        self.paint_markers(ui, rect, &markers, hovered);
        if response.clicked()
            && !response.double_clicked()
            && let Some(c) = hovered
        {
            self.select_cluster(c);
        }

        self.map.zoom_buttons(ui, rect);
        let mut attribution = "Map: Natural Earth".to_string();
        if let Some(o) = self.overlay.as_ref().filter(|o| !o.attribution.is_empty()) {
            attribution = format!("{}  ·  {attribution}", o.attribution);
        }
        let galley = painter.layout_no_wrap(attribution, FontId::proportional(10.5), theme::COMMENT);
        let r = Rect::from_min_size(rect.right_bottom() - galley.size() - vec2(10.0, 6.0), galley.size())
            .expand2(vec2(5.0, 2.0));
        painter.rect_filled(r, 0.0, theme::BG_DARK.gamma_multiply(0.8));
        painter.galley(r.min + vec2(5.0, 2.0), galley, theme::COMMENT);

        self.empty_state(ui, rect);
    }

    /// Centred card for errors, the first run, and libraries without GPS data.
    fn empty_state(&mut self, ui: &mut Ui, rect: Rect) {
        let (text, open_button) = if let Some(m) = &self.message {
            (m.clone(), false)
        } else if self.config.root.is_none() {
            ("Choose the folder with your iPhone photos\nto see where they were taken.".into(), true)
        } else if self.scan.is_none() && self.library.as_ref().is_some_and(|l| l.items.is_empty()) {
            ("None of the photos in this folder have location data.".into(), true)
        } else {
            return;
        };
        let card = Rect::from_center_size(rect.center(), vec2(420.0, 132.0));
        let inner = theme::frame(ui.painter(), card.expand(4.0), theme::BG);
        let mut ui = ui.new_child(
            egui::UiBuilder::new().max_rect(inner.shrink(16.0)).layout(egui::Layout::top_down(egui::Align::Center)),
        );
        let color = if open_button { theme::FG } else { ui.visuals().error_fg_color };
        ui.label(RichText::new(text).size(15.0).color(color));
        ui.add_space(10.0);
        if open_button {
            if ui.button("Open Folder…").clicked() {
                self.choose_folder(ui.ctx());
            }
        } else if ui.button("OK").clicked() {
            self.message = None;
        }
    }

    fn visible_markers(&self, rect: Rect) -> Vec<Marker> {
        let (Some(clusters), Some(_)) = (&self.clusters, &self.library) else { return Vec::new() };
        let margin = rect.expand(MARKER * 2.0);
        let mut out: Vec<Marker> = clusters
            .at_zoom(self.map.view.zoom)
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let anchor = self.map.project(rect, c.x, c.y);
                margin.contains(anchor).then(|| Marker { anchor, rect: marker_rect(anchor, 1.0), cluster: i })
            })
            .collect();
        // Southern markers overlap northern ones, like pins on a board.
        out.sort_by(|a, b| a.anchor.y.total_cmp(&b.anchor.y));
        out
    }

    fn paint_markers(&mut self, ui: &Ui, rect: Rect, markers: &[Marker], hovered: Option<usize>) {
        let (Some(clusters), Some(lib)) = (&self.clusters, &self.library) else { return };
        let level = clusters.at_zoom(self.map.view.zoom);
        let painter = ui.painter_at(rect);
        let centre = rect.center();
        let mut deferred = None;
        for m in markers {
            if Some(m.cluster) == hovered {
                deferred = Some(m);
                continue;
            }
            let c = &level[m.cluster];
            let item = &lib.items[c.cover() as usize];
            let priority = m.anchor.distance_sq(centre);
            let tex = self.media.thumb(item, priority);
            paint_marker(&painter, m.anchor, false, tex.as_ref(), c.members.len(), item, self.media.thumb_failed(item));
        }
        // The hovered marker is drawn last and slightly larger.
        if let Some(m) = deferred {
            let c = &level[m.cluster];
            let item = &lib.items[c.cover() as usize];
            let tex = self.media.thumb(item, 0.0);
            paint_marker(&painter, m.anchor, true, tex.as_ref(), c.members.len(), item, self.media.thumb_failed(item));
        }
    }

    fn select_cluster(&mut self, c: usize) {
        let (Some(clusters), Some(lib)) = (&self.clusters, &self.library) else { return };
        let cl = &clusters.at_zoom(self.map.view.zoom)[c];
        let items: Vec<&Item> = cl.members.iter().map(|&i| &lib.items[i as usize]).collect();
        let videos = items.iter().filter(|i| i.kind == Kind::Video).count();
        let photos = items.len() - videos;
        let mut counts = Vec::new();
        if photos > 0 {
            counts.push(format!("{} {}", group(photos), if photos == 1 { "Photo" } else { "Photos" }));
        }
        if videos > 0 {
            counts.push(format!("{} {}", group(videos), if videos == 1 { "Video" } else { "Videos" }));
        }
        let newest = items.first().and_then(|i| i.taken_local());
        let oldest = items.last().and_then(|i| i.taken_local());
        let dates = match (oldest, newest) {
            (Some(a), Some(b)) if a.date() == b.date() => a.format("%-d %B %Y").to_string(),
            (Some(a), Some(b)) if a.format("%Y%m").to_string() == b.format("%Y%m").to_string() => {
                format!("{}–{}", a.format("%-d"), b.format("%-d %B %Y"))
            }
            (Some(a), Some(b)) => format!("{} – {}", a.format("%b %Y"), b.format("%b %Y")),
            _ => String::new(),
        };
        self.selection = Some(Selection {
            members: cl.members.clone(),
            title: describe_places(&items),
            subtitle: [counts.join(", "), dates]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("  ·  "),
        });
    }

    fn selection_panel(&mut self, ui: &mut Ui) {
        let (Some(sel), Some(lib)) = (&self.selection, &self.library) else { return };
        let mut close = false;
        let mut open = None;
        let frame =
            egui::Frame::NONE.fill(theme::BG).inner_margin(egui::Margin { left: 14, right: 8, top: 8, bottom: 0 });
        let panel = egui::Panel::right("selection")
            .resizable(true)
            .default_size(380.0)
            .min_size(240.0)
            .frame(frame)
            .show_separator_line(false);
        let shown = panel.show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(&sel.title).font(theme::blackletter(23.0)).color(theme::FG));
                    ui.label(RichText::new(&sel.subtitle).weak());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui.button("✕").on_hover_text("Close").clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(6.0);

            let gap = 3.0;
            let width = ui.available_width();
            let cols = ((width + gap) / (104.0 + gap)).floor().max(2.0) as usize;
            let cell = (width - gap * (cols - 1) as f32) / cols as f32;
            let rows = sel.members.len().div_ceil(cols);
            egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, cell, rows, |ui, range| {
                ui.spacing_mut().item_spacing = vec2(gap, gap);
                for row in range {
                    ui.horizontal(|ui| {
                        for col in 0..cols {
                            let n = row * cols + col;
                            let Some(&idx) = sel.members.get(n) else { break };
                            let item = &lib.items[idx as usize];
                            let (r, resp) = ui.allocate_exact_size(vec2(cell, cell), Sense::click());
                            let tex = self.media.thumb(item, 1e6 + n as f32);
                            paint_tile(ui, r, tex.as_ref(), item, resp.hovered());
                            if resp.clicked() {
                                open = Some(n);
                            }
                        }
                    });
                }
            });
        });
        let r = shown.response.rect;
        theme::rule_v(ui.painter(), r.left(), r.y_range(), true);
        if close {
            self.selection = None;
        } else if let Some(n) = open {
            self.viewer = Some(Viewer::new(sel.members.clone(), n));
        }
    }
}

struct Marker {
    anchor: Pos2,
    rect: Rect,
    cluster: usize,
}

fn marker_rect(anchor: Pos2, scale: f32) -> Rect {
    let s = MARKER * scale;
    Rect::from_center_size(anchor - vec2(0.0, s / 2.0 + 9.0), vec2(s, s))
}

/// A photo in the dungeonOS frame on a short bronze stem, with a brass
/// count square for groups.
fn paint_marker(
    painter: &egui::Painter,
    anchor: Pos2,
    hovered: bool,
    tex: Option<&egui::TextureHandle>,
    count: usize,
    cover: &Item,
    failed: bool,
) {
    let body = marker_rect(anchor, if hovered { 1.1 } else { 1.0 });
    let outer = body.expand(4.0);
    painter.rect_filled(outer.expand(2.0), 0.0, Color32::from_black_alpha(60));
    painter.rect_filled(outer.expand(1.0), 0.0, Color32::from_black_alpha(90));
    painter.add(Shape::convex_polygon(
        vec![pos2(anchor.x - 6.0, outer.bottom() - 1.0), pos2(anchor.x + 6.0, outer.bottom() - 1.0), anchor],
        theme::BRONZE,
        Stroke::new(1.0, theme::BG_DARK),
    ));
    let line = if hovered { theme::BRASS } else { theme::BRONZE };
    theme::frame_with(painter, outer, theme::SURFACE, line);
    match tex {
        Some(t) => {
            painter.image(t.id(), body, media::square_uv(t.size()), Color32::WHITE);
        }
        None if failed => {
            painter.text(body.center(), Align2::CENTER_CENTER, "?", FontId::proportional(18.0), theme::COMMENT);
        }
        None if cover.kind == Kind::Video => play_icon(painter, body.center(), 9.0, theme::COMMENT),
        None => {}
    }
    if count == 1 && cover.kind == Kind::Video {
        let r = Rect::from_min_size(body.left_bottom() + vec2(3.0, -19.0), vec2(16.0, 16.0));
        painter.rect_filled(r, 0.0, theme::BG_DARK.gamma_multiply(0.75));
        play_icon(painter, r.center(), 4.0, theme::FG);
    }
    if count > 1 {
        // Like the active workspace square in waybar: a coloured square in a
        // ring of the panel colour.
        let galley = painter.layout_no_wrap(compact(count), FontId::proportional(13.0), theme::BG_DARK);
        let w = (galley.size().x + 10.0).max(20.0);
        let badge = Rect::from_center_size(outer.right_top() + vec2(-3.0, 3.0), vec2(w, 20.0));
        painter.rect_filled(badge.expand(3.0), 0.0, theme::BRONZE);
        painter.rect_filled(badge.expand(2.0), 0.0, theme::BG_DARK);
        painter.rect_filled(badge, 0.0, theme::BRASS);
        painter.galley(badge.center() - galley.size() / 2.0, galley, theme::BG_DARK);
    }
}

fn paint_tile(ui: &Ui, r: Rect, tex: Option<&egui::TextureHandle>, item: &Item, hovered: bool) {
    let p = ui.painter();
    match tex {
        Some(t) => {
            p.image(t.id(), r, media::square_uv(t.size()), Color32::WHITE);
        }
        None => {
            p.rect_filled(r, 0.0, theme::SURFACE);
        }
    }
    // Returns the text's rect; the dark backing extends 14 pt to the left of
    // it for videos, leaving room for the play icon.
    let label = |text: String, pos: Pos2, align: Align2, color: Color32| {
        let font = FontId::proportional(11.0);
        let galley = p.layout_no_wrap(text, font, color);
        let tr = align.anchor_size(pos, galley.size());
        let pad_left = if item.kind == Kind::Video { 14.0 } else { 4.0 };
        let backing = Rect::from_min_max(tr.min - vec2(pad_left, 1.0), tr.max + vec2(4.0, 1.0));
        p.rect_filled(backing, 0.0, theme::BG_DARK.gamma_multiply(0.75));
        p.galley(tr.min, galley, color);
        tr
    };
    if item.live.is_some() {
        label("LIVE".into(), r.left_top() + vec2(7.0, 5.0), Align2::LEFT_TOP, theme::BRASS);
    }
    if item.favorite {
        p.text(r.right_top() + vec2(-7.0, 4.0), Align2::RIGHT_TOP, "♥", FontId::proportional(14.0), theme::BRASS);
    }
    if item.kind == Kind::Video {
        let d = item.duration.map(viewer::format_duration).unwrap_or_default();
        let text = label(d, r.right_bottom() - vec2(7.0, 5.0), Align2::RIGHT_BOTTOM, theme::FG);
        play_icon(p, text.left_center() - vec2(8.0, 0.0), 4.0, theme::FG);
    }
    if hovered {
        // An inset square, like the waybar hover highlight.
        p.rect_stroke(r, 0.0, Stroke::new(3.0, theme::BG), egui::StrokeKind::Inside);
        p.rect_stroke(r.shrink(3.0), 0.0, Stroke::new(1.0, theme::BRASS), egui::StrokeKind::Inside);
    }
}

/// A right-pointing triangle (the emoji ▶ ignores text colour).
fn play_icon(painter: &egui::Painter, centre: Pos2, r: f32, color: Color32) {
    let pts = vec![centre + vec2(-r * 0.7, -r), centre + vec2(-r * 0.7, r), centre + vec2(r, 0.0)];
    painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// Bounding box (normalised Mercator) of some items.
fn bounds<'a>(items: impl Iterator<Item = &'a Item>) -> Option<(f64, f64, f64, f64)> {
    items.fold(None, |b, it| {
        Some(match b {
            None => (it.x, it.y, it.x, it.y),
            Some((x0, y0, x1, y1)) => (x0.min(it.x), y0.min(it.y), x1.max(it.x), y1.max(it.y)),
        })
    })
}

/// "Cambridge, United Kingdom", or the distance to the nearest notable place.
pub fn place_name(lat: f64, lon: f64) -> String {
    match basemap::get().nearest_city(lat, lon) {
        Some((c, d)) if d < 30.0 => {
            // US places read better with their state than with the country.
            let usa = c.country == "United States of America" && !c.region.is_empty();
            format!("{}, {}", c.name, if usa { &c.region } else { &c.country })
        }
        Some((c, d)) => format!("{} km from {}, {}", group(d.round() as usize), c.name, c.country),
        None => viewer::format_coords(lat, lon),
    }
}

/// Title for a group of photos: the place they were taken, or the most
/// common few ("London, Paris & 2 more") when they span several.
fn describe_places(items: &[&Item]) -> String {
    const SAMPLE: usize = 400;
    let step = items.len().div_ceil(SAMPLE).max(1);
    let mut counts: Vec<(String, usize)> = Vec::new();
    for it in items.iter().step_by(step) {
        let name = match basemap::get().nearest_city(it.lat, it.lon) {
            Some((c, d)) if d < 30.0 => c.name.clone(),
            _ => place_name(it.lat, it.lon),
        };
        match counts.iter_mut().find(|(n, _)| *n == name) {
            Some((_, k)) => *k += 1,
            None => counts.push((name, 1)),
        }
    }
    counts.sort_by_key(|(_, k)| std::cmp::Reverse(*k));
    match counts.as_slice() {
        [] => String::new(),
        [_] => place_name(items[0].lat, items[0].lon),
        [(a, _), (b, _)] => format!("{a} & {b}"),
        [(a, _), (b, _), rest @ ..] if step == 1 => format!("{a}, {b} & {} more", rest.len()),
        [(a, _), (b, _), ..] => format!("{a}, {b} & more"),
    }
}

/// 1234567 → "1,234,567".
fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Badge text: 999, 1.2K, 12K, 1.3M.
fn compact(n: usize) -> String {
    match n {
        0..=999 => n.to_string(),
        1000..=9_999 => format!("{:.1}K", n as f64 / 1000.0),
        10_000..=999_999 => format!("{}K", n / 1000),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

impl eframe::App for MappaMundiApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.media.begin_frame(&ctx);
        self.basemap.cache.begin_frame(&ctx);
        if let Some(o) = &mut self.overlay {
            o.cache.begin_frame(&ctx);
        }
        self.poll_background(ui);

        if let (Some(viewer), Some(lib)) = (&mut self.viewer, &self.library) {
            let mut action = viewer::Action::None;
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                action = viewer.ui(ui, lib, &mut self.media);
            });
            match action {
                viewer::Action::None => {}
                viewer::Action::Close => self.viewer = None,
                viewer::Action::ShowOnMap(idx) => {
                    let item = &lib.items[idx as usize];
                    let to = View { cx: item.x, cy: item.y, zoom: self.map.view.zoom.max(14.0) };
                    self.viewer = None;
                    self.map.fly_to(ui, to);
                }
            }
        } else {
            let frame =
                egui::Frame::NONE.fill(theme::BG).inner_margin(egui::Margin { left: 12, right: 12, top: 5, bottom: 8 });
            let top = egui::Panel::top("top").frame(frame).show_separator_line(false).show(ui, |ui| self.top_bar(ui));
            let r = top.response.rect;
            theme::rule_h(ui.painter(), r.x_range(), r.bottom(), false);
            self.selection_panel(ui);
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.map_ui(ui));
            if self.selection.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.selection = None;
            }
        }

        self.media.end_frame();
        self.basemap.cache.end_frame();
        if let Some(o) = &mut self.overlay {
            o.cache.end_frame();
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            let v = self.map.view;
            self.config.view = Some((v.cx, v.cy, v.zoom));
            self.config.save();
        }
    }
}
