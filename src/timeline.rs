//! The timeline: every photo and video, newest first, under month headings,
//! with each day's photos grouped under its date and place.

use std::collections::HashMap;

use chrono::{Datelike, NaiveDate};
use egui::text::{LayoutJob, TextWrapping};
use egui::{Align2, Color32, FontId, Painter, Rect, Sense, Ui, pos2, vec2};

use crate::app::{describe_places, group, paint_tile};
use crate::library::{Item, Kind, Library};
use crate::media::Media;
use crate::theme;

/// Thumbnail edge the grid aims for, in points.
const CELL: f32 = 132.0;
const GAP: f32 = 3.0;
const MONTH_H: f32 = 62.0;
/// A day's heading: its date, and below that where it was.
const DAY_H: f32 = 40.0;
/// Space between days sharing a line.
const DAY_GAP: f32 = 18.0;
/// The column of years on the right.
const RAIL_W: f32 = 72.0;
const MARGIN: f32 = 16.0;

/// Photos taken on one day, or (`date: None`) all those without a date.
struct Day {
    date: Option<NaiveDate>,
    /// Range in `Timeline::order`.
    start: usize,
    len: usize,
}

struct Month {
    date: Option<NaiveDate>,
    first_day: usize,
    photos: usize,
    videos: usize,
}

/// Part of a day on one line of the grid: thumbnails `start..start + len`
/// of `order`, `x` points from the left, with the day's heading above them
/// on its first line.
struct Block {
    x: f32,
    day: usize,
    start: usize,
    len: usize,
    heading: bool,
}

enum Row {
    Month(usize),
    Line(Vec<Block>),
}

pub struct Timeline {
    /// Item indices, newest first; undated ones last.
    pub order: Vec<u32>,
    days: Vec<Day>,
    months: Vec<Month>,
    /// Layout for `width`: each row's top (in content coordinates), height
    /// and contents, and where each month's heading is.
    rows: Vec<(f32, f32, Row)>,
    month_y: Vec<f32>,
    width: f32,
    cell: f32,
    height: f32,
    /// Place names of the days shown so far.
    places: HashMap<usize, String>,
    scroll_to: Option<f32>,
}

impl Timeline {
    pub fn new(lib: &Library) -> Self {
        let mut order: Vec<u32> = (0..lib.items.len() as u32).collect();
        // Newest first; `None` sorts below any date, so undated ones go last.
        order.sort_by(|&a, &b| {
            let (a, b) = (&lib.items[a as usize], &lib.items[b as usize]);
            b.taken.cmp(&a.taken).then_with(|| a.path.cmp(&b.path))
        });
        let mut days: Vec<Day> = Vec::new();
        let mut months: Vec<Month> = Vec::new();
        for (n, &i) in order.iter().enumerate() {
            let item = &lib.items[i as usize];
            let date = item.taken_local().map(|t| t.date());
            match days.last_mut() {
                Some(d) if d.date == date => d.len += 1,
                _ => days.push(Day { date, start: n, len: 1 }),
            }
            let month = date.map(|d| (d.year(), d.month()));
            if months.last().is_none_or(|m| m.date.map(|d| (d.year(), d.month())) != month) {
                months.push(Month { date, first_day: days.len() - 1, photos: 0, videos: 0 });
            }
            let m = months.last_mut().unwrap();
            if item.kind == Kind::Video {
                m.videos += 1;
            } else {
                m.photos += 1;
            }
        }
        Self {
            order,
            days,
            months,
            rows: Vec::new(),
            month_y: Vec::new(),
            width: 0.0,
            cell: CELL,
            height: 0.0,
            places: HashMap::new(),
            scroll_to: None,
        }
    }

    /// Lays the rows out for a grid `width` points wide. Days that fit share
    /// a line; longer ones get lines of their own.
    fn layout(&mut self, width: f32) {
        if (width - self.width).abs() < 0.5 && !self.rows.is_empty() {
            return;
        }
        self.width = width;
        let cols = ((width + GAP) / (CELL + GAP)).floor().max(1.0) as usize;
        let cell = (width - GAP * (cols - 1) as f32) / cols as f32;
        self.cell = cell;
        let span = |n: usize| n as f32 * (cell + GAP) - GAP;
        let line_h = |blocks: &[Block]| cell + GAP + if blocks.iter().any(|b| b.heading) { DAY_H } else { 0.0 };

        let (rows, month_y) = (&mut self.rows, &mut self.month_y);
        rows.clear();
        month_y.clear();
        let mut y = 0.0;
        let mut line: Vec<Block> = Vec::new();
        let mut x = 0.0;
        let flush = |line: &mut Vec<Block>, x: &mut f32, y: &mut f32, rows: &mut Vec<(f32, f32, Row)>| {
            if !line.is_empty() {
                let h = line_h(line);
                rows.push((*y, h, Row::Line(std::mem::take(line))));
                *y += h;
            }
            *x = 0.0;
        };
        let mut next_month = 0;
        for (d, day) in self.days.iter().enumerate() {
            if self.months.get(next_month).is_some_and(|m| m.first_day == d) {
                flush(&mut line, &mut x, &mut y, rows);
                month_y.push(y);
                rows.push((y, MONTH_H, Row::Month(next_month)));
                y += MONTH_H;
                next_month += 1;
            }
            let heading = day.date.is_some();
            if day.len <= cols {
                if x > 0.0 && x + span(day.len) > width {
                    flush(&mut line, &mut x, &mut y, rows);
                }
                line.push(Block { x, day: d, start: day.start, len: day.len, heading });
                x += span(day.len) + DAY_GAP;
            } else {
                flush(&mut line, &mut x, &mut y, rows);
                for (k, start) in (day.start..day.start + day.len).step_by(cols).enumerate() {
                    let len = cols.min(day.start + day.len - start);
                    let block = Block { x: 0.0, day: d, start, len, heading: heading && k == 0 };
                    let h = line_h(std::slice::from_ref(&block));
                    rows.push((y, h, Row::Line(vec![block])));
                    y += h;
                }
            }
        }
        flush(&mut line, &mut x, &mut y, rows);
        self.height = y + MARGIN;
    }

    /// Draws the timeline; returns the position in `order` of a photo that
    /// was clicked.
    pub fn ui(&mut self, ui: &mut Ui, lib: &Library, media: &mut Media) -> Option<usize> {
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, theme::BG_DARK);
        if self.order.is_empty() {
            let text = "There are no photos in this folder.";
            ui.painter().text(full.center(), Align2::CENTER_CENTER, text, FontId::proportional(15.0), theme::MUTED);
            return None;
        }
        let rail = Rect::from_min_max(pos2(full.right() - RAIL_W, full.top()), full.max);
        let grid = Rect::from_min_max(full.min + vec2(MARGIN, 0.0), pos2(rail.left() - 4.0, full.bottom()));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(grid));
        let mut area = egui::ScrollArea::vertical().id_salt("timeline").auto_shrink(false);
        if let Some(y) = self.scroll_to.take() {
            area = area.vertical_scroll_offset(y);
        }
        let mut open = None;
        let out = area.show_viewport(&mut child, |ui, viewport| {
            // The scroll bar takes some of the width.
            self.layout(ui.available_width() - 10.0);
            ui.set_height(self.height);
            let origin = ui.max_rect().min;
            let width = self.width;
            let Self { order, days, months, rows, cell, places, .. } = self;
            let painter = ui.painter().clone();
            let first = rows.partition_point(|(y, h, _)| y + h < viewport.top());
            for (n, (y, _, row)) in rows.iter().enumerate().skip(first) {
                // Thumbnails for the next screenful load too, after the visible ones.
                if *y > viewport.bottom() + viewport.height() {
                    break;
                }
                let visible = *y <= viewport.bottom();
                let top = origin.y + y;
                match row {
                    Row::Month(m) if visible => {
                        let month = &months[*m];
                        let base = pos2(origin.x, top + MONTH_H - 16.0);
                        let font = theme::blackletter(27.0);
                        let r = painter.text(base, Align2::LEFT_BOTTOM, month_name(month), font, theme::FG);
                        let count = count_text(month.photos, month.videos);
                        let pos = pos2(r.right() + 14.0, base.y - 4.0);
                        painter.text(pos, Align2::LEFT_BOTTOM, count, FontId::proportional(12.5), theme::MUTED);
                        theme::rule_h(&painter, (origin.x..=origin.x + width).into(), base.y + 6.0, false);
                    }
                    Row::Month(_) => {}
                    Row::Line(blocks) => {
                        let tiles_top = top + if blocks.iter().any(|b| b.heading) { DAY_H } else { 0.0 };
                        for block in blocks {
                            let left = origin.x + block.x;
                            let span = block.len as f32 * (*cell + GAP) - GAP;
                            if visible && block.heading {
                                let day = &days[block.day];
                                let place = places.entry(block.day).or_insert_with(|| {
                                    let items: Vec<&Item> = order[day.start..day.start + day.len]
                                        .iter()
                                        .map(|&i| &lib.items[i as usize])
                                        .filter(|it| it.has_location())
                                        .collect();
                                    describe_places(&items, 40)
                                });
                                // The date, and the place under it, cut to the block's width.
                                let date = day.date.map(|d| d.format("%A %-d").to_string()).unwrap_or_default();
                                let font = FontId::proportional(13.5);
                                let g = fit(&painter, date, font, theme::FG, span.max(CELL));
                                painter.galley(pos2(left, top + 3.0), g, theme::FG);
                                let g = fit(&painter, place.clone(), FontId::proportional(11.5), theme::MUTED, span);
                                painter.galley(pos2(left, top + 21.0), g, theme::MUTED);
                            }
                            for k in 0..block.len {
                                let n_item = block.start + k;
                                let item = &lib.items[order[n_item] as usize];
                                let priority = n as f32 + if visible { 0.0 } else { 1e5 };
                                let tex = media.thumb(item, priority);
                                if !visible {
                                    continue;
                                }
                                let r = Rect::from_min_size(
                                    pos2(left + k as f32 * (*cell + GAP), tiles_top),
                                    vec2(*cell, *cell),
                                );
                                let resp = ui.interact(r, ui.scope_id().with(("tile", n_item)), Sense::click());
                                paint_tile(ui, r, tex.as_ref(), item, resp.hovered());
                                if resp.clicked() {
                                    open = Some(n_item);
                                }
                            }
                        }
                    }
                }
            }
            // Once its heading has scrolled away, the month stays pinned at
            // the top, pushed up by the next month's heading.
            let m = self.month_y.partition_point(|&y| y <= viewport.top()).saturating_sub(1);
            if viewport.top() > self.month_y[m] + MONTH_H - 24.0 {
                let band_h = 40.0;
                let next = self.month_y.get(m + 1).map_or(f32::MAX, |&y| y - viewport.top());
                let band_top = ui.clip_rect().top() - (band_h - next).max(0.0);
                let band = Rect::from_min_size(pos2(origin.x, band_top), vec2(width, band_h));
                painter.rect_filled(band.expand2(vec2(MARGIN, 0.0)), 0.0, theme::BG_DARK);
                // Clicks on the band shouldn't open the photo underneath.
                ui.interact(band, ui.scope_id().with("month band"), Sense::click());
                let month = &self.months[m];
                let base = band.left_bottom() - vec2(0.0, 10.0);
                let font = theme::blackletter(21.0);
                let r = painter.text(base, Align2::LEFT_BOTTOM, month_name(month), font, theme::FG);
                let count = count_text(month.photos, month.videos);
                let font = FontId::proportional(12.0);
                painter.text(r.right_bottom() + vec2(12.0, -3.0), Align2::LEFT_BOTTOM, count, font, theme::MUTED);
                theme::rule_h(&painter, band.x_range(), band.bottom(), false);
            }
        });
        self.year_rail(ui, rail, out.state.clamped_offset().y);
        open
    }

    /// Years down the right-hand side; clicking one jumps to it.
    fn year_rail(&mut self, ui: &mut Ui, rail: Rect, offset: f32) {
        let painter = ui.painter();
        painter.rect_filled(rail, 0.0, theme::BG);
        theme::rule_v(painter, rail.left(), rail.y_range(), true);
        let mut years: Vec<(Option<i32>, usize)> = Vec::new();
        for (m, month) in self.months.iter().enumerate() {
            let year = month.date.map(|d| d.year());
            if years.last().is_none_or(|(y, _)| *y != year) {
                years.push((year, m));
            }
        }
        let current = self.month_y.partition_point(|&y| y <= offset + 1.0).saturating_sub(1);
        let current_year = self.months.get(current).and_then(|m| m.date.map(|d| d.year()));
        let step = ((rail.height() - 24.0) / years.len() as f32).min(30.0);
        for (i, (year, m)) in years.iter().enumerate() {
            let r = Rect::from_min_size(pos2(rail.left(), rail.top() + 12.0 + i as f32 * step), vec2(RAIL_W, step));
            let resp = ui.interact(r, ui.scope_id().with(("year", i)), Sense::click());
            let here = *year == current_year && (year.is_some() || current >= *m);
            let color = if resp.hovered() || here { theme::BRASS } else { theme::MUTED };
            let label = year.map_or("Undated".into(), |y| y.to_string());
            let size = if year.is_some() { 13.0 } else { 10.5 };
            ui.painter().text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(size), color);
            if here {
                let mark = Rect::from_center_size(pos2(r.left() + 8.0, r.center().y), vec2(4.0, 4.0));
                ui.painter().rect_filled(mark, 0.0, theme::BRASS);
            }
            if resp.clicked() {
                self.scroll_to = Some(self.month_y[*m]);
                ui.ctx().request_repaint();
            }
            resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
    }
}

fn month_name(month: &Month) -> String {
    month.date.map_or("Undated".into(), |d| d.format("%B %Y").to_string())
}

/// `text` on one line, cut short with "…" if wider than `width`.
fn fit(painter: &Painter, text: String, font: FontId, color: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::simple_singleline(text, font, color);
    job.wrap = TextWrapping::truncate_at_width(width);
    painter.layout_job(job)
}

/// "312 photos · 4 videos".
fn count_text(photos: usize, videos: usize) -> String {
    let mut parts = Vec::new();
    if photos > 0 {
        parts.push(format!("{} {}", group(photos), if photos == 1 { "photo" } else { "photos" }));
    }
    if videos > 0 {
        parts.push(format!("{} {}", group(videos), if videos == 1 { "video" } else { "videos" }));
    }
    parts.join("  ·  ")
}
