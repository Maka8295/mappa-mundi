//! The dungeonOS look (https://github.com/Maka8295/Dotfiles): warm
//! stone-and-bronze palette, blackletter headings, and square double-ruled
//! frames.

use std::process::Command;
use std::sync::Arc;

use egui::{Color32, CornerRadius, FontData, FontFamily, FontId, Painter, Rect, Shadow, Stroke, TextStyle};

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const BG_DARK: Color32 = hex(0x141210);
pub const BG: Color32 = hex(0x1A1816);
pub const SURFACE: Color32 = hex(0x24201C);
pub const BAND: Color32 = hex(0x463D34);
pub const BRONZE: Color32 = hex(0x7E705D);
pub const FG: Color32 = hex(0xC8B8A0);
pub const MUTED: Color32 = hex(0x9D8E7C);
pub const COMMENT: Color32 = hex(0x6F675E);
pub const MOSS: Color32 = hex(0x6E7F5A);
pub const BRASS: Color32 = hex(0xA88A5B);

const BLACKLETTER: &str = "blackletter";

/// Blackletter (CloisterBlack) when installed, otherwise the body font.
pub fn blackletter(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BLACKLETTER.into()))
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.text_styles.insert(TextStyle::Heading, blackletter(22.0));
        style.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        style.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        style.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
        style.spacing.button_padding = egui::vec2(8.0, 3.0);

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = BG;
        v.window_fill = BG;
        v.extreme_bg_color = BG_DARK;
        v.faint_bg_color = SURFACE;
        v.code_bg_color = SURFACE;
        v.hyperlink_color = BRASS;
        v.warn_fg_color = BRASS;
        // The palette's rust (#7B4A3E), lightened enough to read as text.
        v.error_fg_color = hex(0xC0705E);
        v.weak_text_color = Some(MUTED);
        v.window_stroke = Stroke::new(1.0, BRONZE);
        v.window_corner_radius = CornerRadius::ZERO;
        v.menu_corner_radius = CornerRadius::ZERO;
        v.window_shadow = Shadow { offset: [0, 0], blur: 8, spread: 1, color: Color32::from_black_alpha(150) };
        v.popup_shadow = v.window_shadow;
        v.selection.bg_fill = MOSS;
        v.selection.stroke = Stroke::new(1.0, BG_DARK);

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = BG;
        w.noninteractive.weak_bg_fill = BG;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BAND);
        w.noninteractive.fg_stroke = Stroke::new(1.0, FG);
        w.inactive.bg_fill = SURFACE;
        w.inactive.weak_bg_fill = SURFACE;
        w.inactive.bg_stroke = Stroke::new(1.0, BAND);
        w.inactive.fg_stroke = Stroke::new(1.0, FG);
        w.hovered.bg_fill = BAND;
        w.hovered.weak_bg_fill = BAND;
        w.hovered.bg_stroke = Stroke::new(1.0, BRONZE);
        w.hovered.fg_stroke = Stroke::new(1.0, FG);
        w.active.bg_fill = BRASS;
        w.active.weak_bg_fill = BRASS;
        w.active.bg_stroke = Stroke::new(1.0, BRONZE);
        w.active.fg_stroke = Stroke::new(1.0, BG_DARK);
        w.open = w.hovered;
        for wv in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            wv.corner_radius = CornerRadius::ZERO;
            wv.expansion = 0.0;
        }
    });
}

fn fonts() -> egui::FontDefinitions {
    let mut defs = egui::FontDefinitions::default();
    let mut blackletter_chain = Vec::new();
    if let Some(data) = system_font("Cloister Black") {
        defs.font_data.insert("cloister".into(), Arc::new(FontData::from_owned(data)));
        blackletter_chain.push("cloister".to_string());
    }
    if let Some(data) = system_font("FantasqueSansM Nerd Font") {
        defs.font_data.insert("fantasque".into(), Arc::new(FontData::from_owned(data)));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            defs.families.entry(family).or_default().insert(0, "fantasque".into());
        }
    }
    // Japanese, Chinese and Korean place and street names.
    if let Some(data) = system_font("Noto Sans CJK JP") {
        defs.font_data.insert("cjk".into(), Arc::new(FontData::from_owned(data)));
        defs.families.entry(FontFamily::Proportional).or_default().push("cjk".into());
    }
    // Anything CloisterBlack lacks (digits look odd in it, accents) falls
    // through to the body font.
    blackletter_chain.extend(defs.families[&FontFamily::Proportional].iter().cloned());
    defs.families.insert(FontFamily::Name(BLACKLETTER.into()), blackletter_chain);
    defs
}

/// Reads an installed font by family name via fontconfig.
fn system_font(family: &str) -> Option<Vec<u8>> {
    let out = Command::new("fc-match").arg("--format=%{family}\n%{file}").arg(family).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (found, file) = text.split_once('\n')?;
    // fc-match always answers with *something*; only accept the real thing.
    if !found.to_lowercase().contains(&family.to_lowercase()) {
        eprintln!("mappa-mundi: font '{family}' not installed, using the default");
        return None;
    }
    std::fs::read(file.trim()).ok()
}

/// Paints the dungeonOS frame (1 px bronze line, 2 px dark gap, 1 px band)
/// around `rect`, filled with `fill`, and returns the area inside it.
pub fn frame(painter: &Painter, rect: Rect, fill: Color32) -> Rect {
    frame_with(painter, rect, fill, BRONZE)
}

/// [`frame`] with a different outer line colour (e.g. brass when hovered).
pub fn frame_with(painter: &Painter, rect: Rect, fill: Color32, line: Color32) -> Rect {
    painter.rect_filled(rect, 0.0, line);
    painter.rect_filled(rect.shrink(1.0), 0.0, BG_DARK);
    painter.rect_filled(rect.shrink(3.0), 0.0, BAND);
    let inner = rect.shrink(4.0);
    painter.rect_filled(inner, 0.0, fill);
    inner
}

/// The same double rule as a horizontal edge between two areas. `down`
/// says which side the band (the inner line) faces.
pub fn rule_h(painter: &Painter, x: egui::Rangef, y: f32, down: bool) {
    let s = if down { 1.0 } else { -1.0 };
    painter.hline(x, y + 0.5 * s, Stroke::new(1.0, BRONZE));
    painter.hline(x, y + 2.0 * s, Stroke::new(2.0, BG_DARK));
    painter.hline(x, y + 3.5 * s, Stroke::new(1.0, BAND));
}

/// Vertical version of [`rule_h`]; `right` says which side the band faces.
pub fn rule_v(painter: &Painter, x: f32, y: egui::Rangef, right: bool) {
    let s = if right { 1.0 } else { -1.0 };
    painter.vline(x + 0.5 * s, y, Stroke::new(1.0, BRONZE));
    painter.vline(x + 2.0 * s, y, Stroke::new(2.0, BG_DARK));
    painter.vline(x + 3.5 * s, y, Stroke::new(1.0, BAND));
}
